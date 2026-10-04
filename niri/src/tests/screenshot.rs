use std::sync::Arc;

use niri_ipc::{Action, Response};
use smithay::wayland::selection::data_device::{
    current_data_device_selection_userdata, set_data_device_selection,
};

use super::Fixture;

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
    let id = Some(u64::MAX);
    let err = f.ipc_action(capture(id, None, true)).unwrap_err();
    assert!(err.contains("does not exist"));
    assert!(matches!(
        f.ipc_action(capture(id, None, false)),
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
        let path =
            std::env::temp_dir().join(format!("niri-capture-{:016x}.png", fastrand::u64(..)));
        let path = path.to_str().unwrap().to_owned();

        f.niri_state().backend.headless().add_renderer().unwrap();

        let reply = f
            .ipc_action(capture(Some(id), Some(path.clone()), true))
            .unwrap();
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
        let err = f
            .ipc_action(capture(Some(id), Some(directory), true))
            .unwrap_err();
        assert!(err.contains("error writing screenshot"));
    }
}
