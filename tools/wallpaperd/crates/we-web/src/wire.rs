use anyhow::{Result, bail, ensure};
use serde_json::Value;
use std::{
    io,
    mem::{self, size_of, size_of_val},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

pub(crate) const MAX_MESSAGE: usize = 64 * 1024;
pub(crate) const MAX_FDS: usize = 5;

pub(crate) fn pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

pub(crate) fn send(socket: RawFd, value: &Value, fds: &[RawFd]) -> Result<()> {
    ensure!(fds.len() <= MAX_FDS, "too many Web host descriptors");
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_MESSAGE, "oversized Web host message");
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let control_len = unsafe { libc::CMSG_SPACE(size_of_val(fds) as u32) } as usize;
    let mut control = vec![0u8; control_len];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    if !fds.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len();
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size_of_val(fds) as u32) as usize;
            std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(header).cast(), fds.len());
        }
    }
    let sent = unsafe { libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
    if sent < 0 {
        return Err(io::Error::last_os_error().into());
    }
    ensure!(sent as usize == bytes.len(), "short Web host message");
    Ok(())
}

pub(crate) fn receive(socket: RawFd) -> Result<(Value, Vec<OwnedFd>)> {
    let mut bytes = vec![0u8; MAX_MESSAGE + 1];
    let mut control =
        vec![0u8; unsafe { libc::CMSG_SPACE((MAX_FDS * size_of::<RawFd>()) as u32) } as usize];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len();
    let read = unsafe { libc::recvmsg(socket, &mut message, libc::MSG_CMSG_CLOEXEC) };
    if read == 0 {
        bail!(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Web host closed"
        ));
    }
    if read < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut descriptors = Vec::new();
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            ensure!(
                (*header).cmsg_len >= libc::CMSG_LEN(0) as usize,
                "invalid Web host control length"
            );
            ensure!(
                (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS,
                "invalid Web host control message"
            );
            let bytes = (*header).cmsg_len - libc::CMSG_LEN(0) as usize;
            ensure!(
                bytes.is_multiple_of(size_of::<RawFd>()),
                "invalid descriptor payload"
            );
            let count = bytes / size_of::<RawFd>();
            for index in 0..count {
                let fd = *libc::CMSG_DATA(header).cast::<RawFd>().add(index);
                descriptors.push(OwnedFd::from_raw_fd(fd));
            }
            ensure!(
                descriptors.len() <= MAX_FDS,
                "too many Web host descriptors"
            );
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    ensure!(
        message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) == 0,
        "truncated Web host message"
    );
    bytes.truncate(read as usize);
    Ok((serde_json::from_slice(&bytes)?, descriptors))
}

pub(crate) fn raw(fds: &[OwnedFd]) -> Vec<RawFd> {
    fds.iter().map(AsRawFd::as_raw_fd).collect()
}

pub(crate) fn frame_value(info: &crate::frames::Info) -> Value {
    serde_json::json!({
        "sequence": info.sequence, "size": info.size, "visible": info.visible,
        "format": info.format, "modifier": info.modifier,
        "planes": info.planes.iter().map(|p| serde_json::json!({"stride":p.stride,"offset":p.offset})).collect::<Vec<_>>()
    })
}

pub(crate) fn frame_info(value: &Value) -> Result<crate::frames::Info> {
    let array = |key: &str| -> Result<Vec<u64>> {
        value[key]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid Web frame {key}"))?
            .iter()
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| anyhow::anyhow!("invalid Web frame {key}"))
            })
            .collect()
    };
    let size = array("size")?;
    let visible = array("visible")?;
    ensure!(
        size.len() == 2 && visible.len() == 4,
        "invalid Web frame geometry"
    );
    let planes = value["planes"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid Web frame planes"))?
        .iter()
        .map(|plane| {
            Ok(crate::frames::Plane {
                stride: u32::try_from(
                    plane["stride"]
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("invalid stride"))?,
                )?,
                offset: plane["offset"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("invalid offset"))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !planes.is_empty() && planes.len() <= 4,
        "invalid Web frame planes"
    );
    Ok(crate::frames::Info {
        sequence: value["sequence"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("invalid sequence"))?,
        size: [u32::try_from(size[0])?, u32::try_from(size[1])?],
        visible: [
            u32::try_from(visible[0])?,
            u32::try_from(visible[1])?,
            u32::try_from(visible[2])?,
            u32::try_from(visible[3])?,
        ],
        format: u32::try_from(
            value["format"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("invalid format"))?,
        )?,
        modifier: value["modifier"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("invalid modifier"))?,
        planes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn seqpacket_preserves_message_and_descriptor_boundary() {
        let (left, right) = pair().unwrap();
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(fd >= 0);
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        send(
            left.as_raw_fd(),
            &serde_json::json!({"sequence":7}),
            &[fd.as_raw_fd()],
        )
        .unwrap();
        let (value, descriptors) = receive(right.as_raw_fd()).unwrap();
        assert_eq!(value["sequence"], 7);
        assert_eq!(descriptors.len(), 1);
        assert_ne!(descriptors[0].as_raw_fd(), fd.as_raw_fd());
    }
}
