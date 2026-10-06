use cef::*;

wrap_app! {
    pub(crate) struct WebApp { platform: Option<String> }
    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(command) = command_line {
                if let Some(platform) = &self.platform {
                    command.append_switch_with_value(Some(&"ozone-platform".into()), Some(&platform.as_str().into()));
                    if platform != "headless" {
                        command.append_switch_with_value(Some(&"use-angle".into()), Some(&"gl-egl".into()));
                    }
                }
                command.append_switch_with_value(
                    Some(&"autoplay-policy".into()),
                    Some(&"no-user-gesture-required".into()),
                );
                // Headless Ozone cannot import decoded platform video buffers;
                // software decoding keeps video from crashing the shared WebGL GPU process.
                command.append_switch(Some(&"disable-accelerated-video-decode".into()));
                for name in [
                    "disable-background-timer-throttling",
                    "disable-component-update",
                    "no-first-run",
                    "allow-file-access-from-files",
                ] {
                    command.append_switch(Some(&name.into()));
                }
                // Each isolated worker hosts wallpapers, not a tabbed browser.
                // Keep network/audio services on their own threads in this host,
                // without another libcef mapping or an unused prewarmed renderer.
                for (switch, features) in [
                    ("enable-features", &["NetworkServiceInProcess2"][..]),
                    ("disable-features", &["SpareRendererForSitePerProcess", "AudioServiceOutOfProcess"][..]),
                ] {
                    let name = CefString::from(switch);
                    let mut list = CefString::from(&command.switch_value(Some(&name))).to_string();
                    for feature in features {
                        if !list.split(',').any(|value| value == *feature) {
                            if !list.is_empty() { list.push(','); }
                            list.push_str(feature);
                        }
                    }
                    command.append_switch_with_value(Some(&name), Some(&list.as_str().into()));
                }
            }
        }
        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(Renderer::new())
        }
    }
}

wrap_render_process_handler! {
    struct Renderer;
    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let (Some(frame), Some(context)) = (frame, context) else {
                return;
            };
            if frame.is_main() == 0 {
                return;
            }
            if let Some(global) = context.global() {
                let mut host = Host::new(frame.clone());
                if let Some(mut function) =
                    v8_value_create_function(Some(&"host".into()), Some(&mut host))
                {
                    global.set_value_bykey(
                        Some(&"__wallpaperdHost".into()),
                        Some(&mut function),
                        V8Propertyattribute::default(),
                    );
                }
                evaluate(context, frame, include_str!("bridge.js"));
            }
        }
        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            let (Some(frame), Some(message)) = (frame, message) else {
                return 0;
            };
            if source_process != ProcessId::BROWSER
                || CefString::from(&message.name()).to_string() != "wallpaperd-state"
                || frame.is_main() == 0
            {
                return 0;
            }
            if let (Some(context), Some(arguments)) = (frame.v8_context(), message.argument_list())
                && context.enter() != 0
            {
                evaluate(
                    &context,
                    frame,
                    &format!(
                        "window.__wallpaperdReceive({})",
                        CefString::from(&arguments.string(0))
                    ),
                );
                context.exit();
            }
            1
        }
    }
}

fn evaluate(context: &V8Context, frame: &Frame, script: &str) {
    let mut value = None;
    let mut exception = None;
    if context.eval(
        Some(&script.into()),
        Some(&CefString::from(&frame.url())),
        1,
        Some(&mut value),
        Some(&mut exception),
    ) == 0
    {
        eprintln!(
            "we-web bridge: {}",
            exception
                .map(|value| CefString::from(&value.message()).to_string())
                .unwrap_or_default()
        );
    }
}

wrap_v8_handler! {
    struct Host {
        frame: Frame,
    }
    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> i32 {
            if let Some(args) = arguments
                && (args.len() == 1 || args.len() == 2)
                && let Some(name) = &args[0]
                && name.is_string() != 0
                && let Some(mut message) = process_message_create(Some(&"wallpaperd-event".into()))
                && let Some(list) = message.argument_list()
            {
                list.set_string(0, Some(&CefString::from(&name.string_value())));
                if let Some(Some(revision)) = args.get(1)
                    && revision.is_int() != 0
                {
                    list.set_int(1, revision.int_value());
                }
                self.frame
                    .send_process_message(ProcessId::BROWSER, Some(&mut message));
            }
            1
        }
    }
}
