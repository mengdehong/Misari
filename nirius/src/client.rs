// Copyright (C) 2025  Tassilo Horn <tsdh@gnu.org>
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
// FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General Public License for
// more details.
//
// You should have received a copy of the GNU General Public License along with
// this program.  If not, see <https://www.gnu.org/licenses/>.

use std::os::unix::net::UnixStream;

pub fn send_nirius_cmd(cmd: crate::cmds::NiriusCmd) -> Result<String, String> {
    let stream = UnixStream::connect(crate::util::get_nirius_socket_path())
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&stream, &cmd).map_err(|e| e.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;
    serde_json::from_reader::<_, Result<String, String>>(&stream)
        .expect("Could not read response from niriusd")
}
