use super::*;

struct InputState;
wayland_client::delegate_noop!(InputState: ignore wayland_client::protocol::wl_registry::WlRegistry);
wayland_client::delegate_noop!(InputState: ignore wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(InputState: ignore wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);
impl
    wayland_client::Dispatch<
        wayland_client::protocol::wl_registry::WlRegistry,
        wayland_client::globals::GlobalListContents,
    > for InputState
{
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_registry::WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &wayland_client::globals::GlobalListContents,
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

struct Input {
    queue: wayland_client::EventQueue<InputState>,
    pointer: wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
}
impl Input {
    fn new(rig: &Rig) -> Self {
        let display = rig
            .env
            .iter()
            .find(|(key, _)| key == "WAYLAND_DISPLAY")
            .unwrap();
        let stream = std::os::unix::net::UnixStream::connect(&display.1).unwrap();
        let conn = wayland_client::Connection::from_socket(stream).unwrap();
        let (globals, queue) =
            wayland_client::globals::registry_queue_init::<InputState>(&conn).unwrap();
        let qh = queue.handle();
        let manager: wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
        let pointer = manager.create_virtual_pointer(None, &qh, ());
        Self { queue, pointer }
    }
    fn motion(&mut self, x: u32, y: u32) {
        self.pointer.motion_absolute(0, x, y, 1000, 1000);
        self.pointer.frame();
        self.queue.roundtrip(&mut InputState).unwrap();
    }
    fn button(&mut self, down: bool) {
        self.pointer.button(
            0,
            0x110,
            if down {
                wayland_client::protocol::wl_pointer::ButtonState::Pressed
            } else {
                wayland_client::protocol::wl_pointer::ButtonState::Released
            },
        );
        self.pointer.frame();
        self.queue.roundtrip(&mut InputState).unwrap();
    }
}

#[test]
#[ignore = "requires Wayland, weston-simple-shm, and WALLPAPERD_TEST_NIRI"]
fn mouse_shader_occlusion_click_scale_pause_and_switch() {
    let mut rig = Rig::new();
    // Fractional output scale exercises logical pointer to physical shader coordinates.
    let output = Command::new(&rig.niri)
        .args(["msg", "output", "winit", "scale", "1.5"])
        .envs(rig.env.iter().cloned())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shader = rig.root().join("mouse.frag");
    fs::write(&shader, "void mainImage(out vec4 c,in vec2 p){ c=vec4(iMouse.xy/iResolution.xy,iMouse.z>0.0?1.0:0.0,1.0); }").unwrap();
    rig.set(&shader, "cut", 20);
    rig.wait_state(|s| s["outputs"]["winit"]["pointer"] == "desktop");
    let mut input = Input::new(&rig);
    input.motion(250, 250);
    rig.wait_corner([64, 191, 0]);
    input.button(true);
    rig.wait_corner([64, 191, 255]);
    input.button(false);
    rig.wait_corner([64, 191, 0]);

    rig.niri_action(&["open-overview"]);
    input.motion(750, 750);
    rig.niri_action(&["close-overview"]);
    input.motion(250, 250);
    rig.wait_corner([64, 191, 0]);

    rig.ok(&["pause"]);
    thread::sleep(Duration::from_millis(200));
    let paused = rig.shot();
    let frames = rig.frames();
    input.motion(750, 750);
    input.button(true);
    input.button(false);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(rig.frames(), frames, "paused mouse shader kept committing");
    assert_eq!(rig.shot(), paused);
    rig.ok(&["resume"]);
    rig.wait_corner([191, 64, 0]);

    // Foreground windows block wallpaper motion and clicks; exposed desktop still responds.
    let window = OwnedProcess(
        Command::new("weston-simple-shm")
            .envs(rig.env.iter().cloned())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    thread::sleep(Duration::from_millis(400));
    rig.niri_action(&["toggle-window-floating"]);
    input.motion(900, 900);
    rig.wait_corner([230, 26, 0]);
    input.motion(500, 500);
    thread::sleep(Duration::from_millis(120));
    rig.wait_corner([230, 26, 0]);
    input.button(true);
    thread::sleep(Duration::from_millis(120));
    rig.wait_corner([230, 26, 0]);
    input.button(false);
    drop(window);
    // Uncovering a stationary pointer must restore input without another movement.
    rig.wait_corner([128, 128, 0]);

    let image = rig.root().join("still.png");
    RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255]))
        .save(&image)
        .unwrap();
    rig.set(&image, "cut", 20);
    rig.wait_state(|s| s["outputs"]["winit"]["pointer"] == "none");
    thread::sleep(Duration::from_millis(150));
    let frames = rig.frames();
    input.motion(100, 100);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(rig.frames(), frames, "mouse motion repainted static image");
    rig.set(&shader, "fade", 20);
    rig.wait_corner([26, 230, 0]);

    input.motion(200, 300);
    rig.wait_corner([51, 179, 0]);
    input.button(true);
    rig.wait_corner([51, 179, 255]);
    input.button(false);
    rig.wait_corner([51, 179, 0]);
}
