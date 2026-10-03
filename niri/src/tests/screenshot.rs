use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use niri_ipc::{Action, Reply, Request, Response};
use smithay::wayland::selection::data_device::{
    current_data_device_selection_userdata, set_data_device_selection,
};

use super::Fixture;
use crate::ipc::server::IpcServer;

fn start_ipc(f: &mut Fixture) -> PathBuf {
    let name = format!("screenshot-test-{:016x}", fastrand::u64(..));
    let server = IpcServer::start(&f.niri().event_loop, Some(OsStr::new(&name))).unwrap();
    let path = server.socket_path.clone().unwrap();
    f.niri().ipc_server = Some(server);
    f.niri_state().ipc_keyboard_layouts_changed();
    path
}

fn request(f: &mut Fixture, socket: &Path, action: Action) -> Reply {
    let socket = socket.to_owned();
    let thread = std::thread::spawn(move || {
        let mut socket = niri_ipc::socket::Socket::connect_to(socket).unwrap();
        socket.send(Request::Action(action)).unwrap()
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !thread.is_finished() {
        assert!(Instant::now() < deadline, "screenshot did not complete");
        f.dispatch();
        std::thread::sleep(Duration::from_millis(1));
    }
    thread.join().unwrap()
}

fn capture(id: Option<u64>, path: Option<String>, wait: bool) -> Action {
    Action::ScreenshotWindow {
        id,
        path,
        silent: true,
        wait,
        write_to_disk: true,
        show_pointer: false,
    }
}

#[test]
fn screenshot_wait_reports_missing_window_and_keeps_legacy_reply() {
    let mut f = Fixture::new();
    let socket = start_ipc(&mut f);
    let id = Some(u64::MAX);
    let err = request(&mut f, &socket, capture(id, None, true)).unwrap_err();
    assert!(err.contains("does not exist"));
    assert!(matches!(
        request(&mut f, &socket, capture(id, None, false)),
        Ok(Response::Handled)
    ));
}

mod egl {
    use super::*;

    #[test]
    fn screenshot_wait_captures_background_window_without_changing_focus() {
        let mut f = Fixture::new();
        f.add_output(1, (1920, 1080));
        f.add_output(2, (1920, 1080));
        let client = f.add_client();
        let window = f.client(client).create_window();
        let surface = window.surface.clone();
        window.commit();
        f.roundtrip(client);
        let window = f.client(client).window(&surface);
        window.attach_new_buffer();
        window.set_size(100, 100);
        window.ack_last_and_commit();
        f.double_roundtrip(client);
        let id = f.niri().layout.focus().unwrap().id().get();
        f.niri_focus_output(2);
        let workspace = f.niri().layout.active_workspace().unwrap().id();
        let focused = f.niri().layout.focus().map(|w| w.id());
        let niri = f.niri();
        set_data_device_selection(
            &niri.display_handle,
            &niri.seat,
            vec!["text/plain".into()],
            Arc::from(&b"keep clipboard"[..]),
        );
        let socket = start_ipc(&mut f);
        let path =
            std::env::temp_dir().join(format!("niri-capture-{:016x}.png", fastrand::u64(..)));
        let path = path.to_str().unwrap().to_owned();

        f.niri_state().backend.headless().add_renderer().unwrap();

        let reply = request(&mut f, &socket, capture(Some(id), Some(path.clone()), true)).unwrap();
        assert!(matches!(reply, Response::ScreenshotSaved { path: Some(p) } if p == path));
        let png = std::fs::read(&path).unwrap();
        let mut decoder = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        assert_eq!((decoder.info().width, decoder.info().height), (100, 100));
        let mut pixels = vec![0; decoder.output_buffer_size().unwrap()];
        decoder.next_frame(&mut pixels).unwrap();
        assert_eq!(f.niri().layout.active_workspace().unwrap().id(), workspace);
        assert_eq!(f.niri().layout.focus().map(|w| w.id()), focused);
        assert_eq!(
            &**current_data_device_selection_userdata(&f.niri().seat).unwrap(),
            b"keep clipboard",
        );
        std::fs::remove_file(path).unwrap();

        let directory = std::env::temp_dir().to_str().unwrap().to_owned();
        let err = request(&mut f, &socket, capture(Some(id), Some(directory), true)).unwrap_err();
        assert!(err.contains("error writing screenshot"));
    }
}
