//! Interaction checks for `--capture-ui`. They drive the real window with
//! keystrokes, clicks, and wheel events through GPUI's input dispatch, then
//! assert on the view and the app core. Each returns its description on success.
use super::window::MainWindow;
use crate::native_core::{get, State};
use crate::native_onboarding::{self as setup, Onboarding, Status};
use gpui::{
    point, px, AppContext, AsyncApp, Context, EntityInputHandler, Focusable, Keystroke, Window,
    WindowHandle,
};
use std::time::Duration;

type Handle = WindowHandle<MainWindow>;

fn view<R>(
    handle: Handle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut MainWindow, &mut Window, &mut Context<MainWindow>) -> R,
) -> Result<R, String> {
    handle.update(cx, f).map_err(|e| e.to_string())
}

async fn settle(cx: &mut AsyncApp) {
    cx.background_executor()
        .timer(Duration::from_millis(200))
        .await;
}

fn key(handle: Handle, cx: &mut AsyncApp, keys: &str) -> Result<(), String> {
    // update_window does not lease the root view, so key handlers can update it.
    cx.update_window(handle.into(), |_, window, cx| {
        for key in keys.split(' ') {
            let keystroke = Keystroke::parse(key).unwrap();
            window.dispatch_keystroke(keystroke.clone(), cx);
            crate::platform::post_key_up(window, &keystroke.key);
        }
    })
    .map_err(|e| e.to_string())
}

fn click(handle: Handle, cx: &mut AsyncApp, x: f32, y: f32) -> Result<(), String> {
    view(handle, cx, |_, window, _| {
        crate::platform::post_click(window, x, y)
    })
}

fn wheel(handle: Handle, cx: &mut AsyncApp, x: f32, y: f32, notches: i16) -> Result<(), String> {
    view(handle, cx, |_, window, _| {
        crate::platform::post_wheel(window, x, y, notches)
    })
}

/// Shows `tab` with the core's current state and no popups.
async fn reset(handle: Handle, cx: &mut AsyncApp, tab: &str) -> Result<(), String> {
    let state = get().snapshot();
    view(handle, cx, |view, window, cx| {
        view.show_scene(state, tab, false, window, cx)
    })?;
    settle(cx).await;
    Ok(())
}

fn scroll_y(handle: Handle, cx: &mut AsyncApp, id: &'static str) -> Result<f32, String> {
    view(handle, cx, |view, _, _| {
        view.scroll.get(id).map_or(0., |h| f32::from(h.offset().y))
    })
}

fn ensure(ok: bool, failure: &str) -> Result<(), String> {
    ok.then_some(()).ok_or_else(|| failure.to_owned())
}

async fn check_overlay(cx: &mut AsyncApp) -> Result<(), String> {
    let owner = cx
        .update(super::overlay::check_owner)
        .map_err(|e| e.to_string())??;
    let foreground = crate::platform::overlay_check_foreground();
    let verify = |visible| {
        crate::app::spawn_blocking(move || {
            crate::platform::check_overlay_window(owner, foreground, visible)
        })
    };
    let shown = async {
        verify(false).await.map_err(|e| e.to_string())??;
        super::overlay::show_listening()?;
        settle(cx).await;
        verify(true).await.map_err(|e| e.to_string())??;
        super::overlay::show_confirmation("added", "Overlay check".into())?;
        settle(cx).await;
        verify(true).await.map_err(|e| e.to_string())??;
        Ok::<(), String>(())
    }
    .await;
    // Always hide after showing, including when a native assertion fails.
    super::overlay::hide()?;
    settle(cx).await;
    let hidden = verify(false).await.map_err(|e| e.to_string())?;
    shown?;
    hidden
}

pub async fn run(
    handle: Handle,
    long_summary: State,
    cx: &mut AsyncApp,
) -> Result<Vec<&'static str>, String> {
    let mut passed = vec![];

    check_overlay(cx).await?;
    passed.push("The visible overlay is topmost, transparent, nonactivating, and a tool window; native hit tests and mouse activation reject interaction");
    passed.push("Queued overlay show, confirmation, and hide preserve the foreground window; hide removes the overlay");

    for (choice, expected) in [
        ("webspeech", "groq"),
        ("local", "local"),
        ("azure-mai", "azure-mai"),
        ("groq", "groq"),
    ] {
        get().action("mode-select", choice.into());
        ensure(
            get().snapshot().mode() == expected,
            &format!("Engine {choice} did not become {expected}"),
        )?;
    }
    get().action("acceleration-select", "cuda".into());
    ensure(
        get().snapshot().setting("whisperAcceleration", "") == "vulkan",
        "CUDA was selectable",
    )?;
    passed.push("Only Groq, Azure MAI, and local Vulkan are selectable");

    reset(handle, cx, "home").await?;
    click(handle, cx, 496., 24.)?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, window, _| {
            view.modal && view.modal_focus[0].is_focused(window)
        })?,
        "The titlebar X did not open the close prompt with focus on its first action",
    )?;
    passed.push("The titlebar X opens the close prompt and focuses its first action");

    for keys in [
        "tab",
        "tab",
        "tab",
        "tab",
        "shift-tab",
        "shift-tab",
        "shift-tab",
    ] {
        key(handle, cx, keys)?;
        ensure(
            view(handle, cx, |view, window, _| {
                view.modal_focus.iter().any(|h| h.is_focused(window))
            })?,
            "Tab moved focus behind the close prompt",
        )?;
    }
    passed.push("Tab and Shift+Tab stay inside the close prompt");

    key(handle, cx, "escape")?;
    settle(cx).await;
    ensure(
        !view(handle, cx, |view, _, _| view.modal)?,
        "Escape left the close prompt open",
    )?;
    passed.push("Escape closes the close prompt");

    view(handle, cx, |_, window, _| {
        crate::platform::post_close(window)
    })?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, window, _| {
            view.modal && view.modal_focus[0].is_focused(window)
        })?,
        "A native close request did not open the close prompt with its first action focused",
    )?;
    key(handle, cx, "escape")?;
    settle(cx).await;
    passed.push(
        "Windows close requests open the close prompt, including the Alt+F4 and taskbar Close path",
    );

    for activation in ["enter", "space"] {
        click(handle, cx, 496., 24.)?;
        settle(cx).await;
        view(handle, cx, |view, window, _| {
            window.focus(&view.modal_focus[2])
        })?;
        settle(cx).await;
        key(handle, cx, activation)?;
        settle(cx).await;
        ensure(
            !view(handle, cx, |view, _, _| view.modal)?,
            &format!("{activation} did not activate the close prompt's Cancel button"),
        )?;
    }
    passed.push("Enter and Space activate the focused close-prompt button");

    click(handle, cx, 496., 24.)?;
    settle(cx).await;
    click(handle, cx, 40., 300.)?;
    settle(cx).await;
    ensure(
        !view(handle, cx, |view, _, _| view.modal)?,
        "Clicking outside the close prompt did not cancel it",
    )?;
    passed.push("Clicking the scrim cancels the close prompt");

    reset(handle, cx, "settings").await?;
    wheel(handle, cx, 300., 300., -2)?;
    settle(cx).await;
    let scrolled = scroll_y(handle, cx, "settings")?;
    ensure(scrolled < 0., "The wheel did not scroll Settings")?;
    passed.push("The wheel scrolls Settings");

    click(handle, cx, 496., 24.)?;
    settle(cx).await;
    wheel(handle, cx, 300., 300., -2)?;
    settle(cx).await;
    ensure(
        scroll_y(handle, cx, "settings")? == scrolled,
        "The page behind the close prompt scrolled",
    )?;
    key(handle, cx, "escape")?;
    settle(cx).await;
    passed.push("The close prompt consumes wheel input meant for the page behind it");

    get().change(|s| s.recording_hotkey = Some("hotkey-btn".into()));
    settle(cx).await;
    key(handle, cx, "escape")?;
    settle(cx).await;
    ensure(
        get().snapshot().recording_hotkey.is_none(),
        "Escape did not cancel shortcut recording",
    )?;
    passed.push("Escape cancels shortcut recording");

    for activation in ["enter", "space"] {
        view(handle, cx, |view, _, cx| {
            view.scroll_to_bottom("settings", cx)
        })?;
        settle(cx).await;
        click(handle, cx, 300., 333.)?;
        settle(cx).await;
        ensure(
            get().snapshot().recording_hotkey.as_deref() == Some("hotkey-btn"),
            "Clicking the shortcut recorder did not start recording",
        )?;
        cx.update_window(handle.into(), |_, window, cx| {
            window.dispatch_keystroke(Keystroke::parse(activation).unwrap(), cx);
        })
        .map_err(|e| e.to_string())?;
        // Simulate a successful capture without registering a global shortcut.
        get().change(|s| s.recording_hotkey = None);
        settle(cx).await;
        view(handle, cx, |_, window, _| {
            crate::platform::post_key_up(window, activation)
        })?;
        settle(cx).await;
        ensure(
            get().snapshot().recording_hotkey.is_none(),
            "Releasing the captured shortcut restarted recording",
        )?;
    }
    passed.push("Releasing captured Enter or Space does not restart shortcut recording");

    reset(handle, cx, "dictionary").await?;
    click(handle, cx, 98., 166.)?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, window, cx| {
            view.inputs
                .dict
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        })?,
        "Clicking the dictionary field's padding did not focus the editor",
    )?;
    key(handle, cx, "z e t a enter")?;
    settle(cx).await;
    let added = get()
        .snapshot()
        .data
        .data
        .dictionary
        .terms
        .iter()
        .any(|t| t.value == "zeta");
    let cleared = view(handle, cx, |view, _, cx| {
        view.inputs.dict.read(cx).text().is_empty()
    })?;
    ensure(
        added && cleared,
        "Typing a term and pressing Enter did not add it",
    )?;
    get().action("dict-remove", "zeta".into());
    passed.push("Clicking field padding focuses the dictionary editor; Enter adds the typed term");

    view(handle, cx, |view, _, cx| {
        view.inputs
            .dict
            .update(cx, |input, cx| input.set_text("abcdef", cx));
    })?;
    settle(cx).await;
    click(handle, cx, 98., 166.)?;
    settle(cx).await;
    key(handle, cx, "z home")?;
    settle(cx).await;
    click(handle, cx, 410., 151.)?;
    settle(cx).await;
    key(handle, cx, "q")?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, cx| {
            view.inputs.dict.read(cx).text() == "zabcdefq"
        })?,
        "Clicks in the field padding did not place the caret at the nearest text position",
    )?;
    view(handle, cx, |_, window, _| {
        crate::platform::post_drag(window, (106., 166.), (510., 166.));
    })?;
    settle(cx).await;
    key(handle, cx, "r")?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, cx| {
            view.inputs.dict.read(cx).text() == "r"
        })?,
        "Dragging past the field boundary did not select its text",
    )?;
    view(handle, cx, |view, _, cx| {
        view.inputs
            .dict
            .update(cx, |input, cx| input.set_text("", cx));
    })?;
    passed
        .push("Padding clicks place the caret and text selection follows drags outside the field");

    let composition = view(handle, cx, |view, window, cx| {
        view.inputs.dict.update(cx, |input, cx| {
            input.set_text("A😊Z", cx);
            input.replace_and_mark_text_in_range(Some(1..3), "e\u{301}日", Some(2..3), window, cx);
            let first = input.text() == "Ae\u{301}日Z"
                && input.marked_text_range(window, cx) == Some(1..4)
                && input.selected_text_range(false, window, cx).unwrap().range == (3..4);
            input.replace_and_mark_text_in_range(None, "é😊", Some(1..3), window, cx);
            let updated = input.text() == "Aé😊Z"
                && input.selected_text_range(false, window, cx).unwrap().range == (2..4);
            input.replace_text_in_range(None, "你😊", window, cx);
            let committed = input.text() == "A你😊Z"
                && input.marked_text_range(window, cx).is_none()
                && input.selected_text_range(false, window, cx).unwrap().range == (4..4);
            input.replace_text_in_range(Some(2..4), "é", window, cx);
            let replaced = input.text() == "A你éZ";
            input.set_text("", cx);
            first && updated && committed && replaced
        })
    })?;
    ensure(
        composition,
        "IME composition or UTF-16 replacement corrupted Unicode text or selection",
    )?;
    passed.push("The live text input handler composes and commits combining marks, CJK, and surrogate pairs with UTF-16 selections");

    get().action("mode-select", "azure-mai".into());
    reset(handle, cx, "settings").await?;
    view(handle, cx, |view, window, cx| {
        let input = view.inputs.endpoint.clone();
        input.update(cx, |input, cx| {
            input.replace_text_in_range(None, "https://example.services.ai.azure.co", window, cx)
        });
        window.focus(&input.read(cx).focus_handle(cx));
    })?;
    settle(cx).await;
    key(handle, cx, "m tab")?;
    settle(cx).await;
    let saved = get().snapshot().setting("azureMaiEndpoint", "");
    get().action("azure-endpoint-input", String::new());
    get().action("mode-select", "groq".into());
    ensure(
        saved == "https://example.services.ai.azure.com",
        &format!("A settings edit did not commit when focus left the field ({saved})"),
    )?;
    passed.push("Settings edits commit when focus leaves the field");

    get().set_setting(setup::KEY, Onboarding::initial(false, "groq").to_value())?;
    get().set_setting("hotkey", serde_json::json!(""))?;
    get().set_setting("liveLanguage", serde_json::json!("auto"))?;
    get().change(|s| {
        s.ready.remove("whisper");
        s.labels.insert("status-text".into(), "Ready".into());
    });
    reset(handle, cx, "home").await?;
    view(handle, cx, |view, window, cx| {
        view.click("ob-engine-local", window, cx);
        view.click("ob-next", window, cx);
        view.click("ob-next", window, cx);
    })?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, cx| {
            let guide = view.setup();
            guide.is_open()
                && guide.engine == "local"
                && guide.step_id() == "install"
                && view.state.mode() == "groq"
                && view.setup_blocker(&guide, cx).is_some()
        })?,
        "Local setup did not wait for the model before switching engines",
    )?;
    passed.push("Choosing Local Whisper waits for the model before switching engines");

    get().change(|s| s.recording_hotkey = Some("hotkey-btn".into()));
    view(handle, cx, |view, window, cx| {
        view.click("ob-exit", window, cx)
    })?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, _| {
            view.setup().status == Status::Paused && view.setup().step_id() == "install"
        })? && get().snapshot().recording_hotkey.is_none(),
        "Exiting setup lost its step or left shortcut recording active",
    )?;
    view(handle, cx, |view, window, cx| {
        view.click("setup-resume-btn", window, cx)
    })?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, _| {
            view.setup().is_open() && view.setup().step_id() == "install"
        })?,
        "Resuming setup did not return to the same step",
    )?;
    passed.push("Exiting setup cancels shortcut recording; Resume returns to the same step");

    get().change(|s| {
        s.ready.insert("whisper".into());
    });
    view(handle, cx, |view, window, cx| {
        view.click("ob-next", window, cx)
    })?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, _| {
            view.setup().step_id() == "microphone" && view.state.mode() == "local"
        })?,
        "Continuing with a downloaded model did not switch to Local Whisper",
    )?;
    passed.push("Continuing after the model is ready selects Local Whisper");

    click(handle, cx, 250., 305.)?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, _| {
            view.open_select == Some("ob-lang-select")
        })?,
        "Clicking the setup language dropdown did not open it",
    )?;
    key(handle, cx, "escape down")?;
    settle(cx).await;
    ensure(
        get().snapshot().setting("liveLanguage", "") == "en"
            && view(handle, cx, |view, _, _| view.open_select.is_none())?,
        "Down did not change the focused language dropdown to English",
    )?;
    key(handle, cx, "up")?;
    settle(cx).await;
    key(handle, cx, "space")?;
    settle(cx).await;
    ensure(
        get().snapshot().setting("liveLanguage", "") == "auto"
            && view(handle, cx, |view, _, _| {
                view.open_select == Some("ob-lang-select")
            })?,
        "Up did not restore Auto-detect, or Space did not reopen the dropdown",
    )?;
    key(handle, cx, "escape")?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, _| view.open_select.is_none())?,
        "Escape left the language dropdown open",
    )?;
    passed.push("Dropdown arrows change the value, Space opens it, and Escape closes it");

    view(handle, cx, |view, window, cx| {
        view.click("ob-next", window, cx);
        view.click("ob-next", window, cx);
    })?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, cx| {
            let guide = view.setup();
            guide.step_id() == "hotkey" && view.setup_blocker(&guide, cx).is_some()
        })?,
        "Setup continued without a saved hotkey",
    )?;
    get().set_setting("hotkey", serde_json::json!("Ctrl+Shift+Space"))?;
    view(handle, cx, |view, window, cx| {
        view.click("ob-next", window, cx)
    })?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, window, cx| {
            view.setup().step_id() == "try"
                && view
                    .inputs
                    .ob_try
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
                && !view.try_done
        })?,
        "The saved hotkey did not enable Try it with its text field focused",
    )?;
    passed.push("Setup requires a saved hotkey before focusing the Try it field");

    get().change(|s| {
        s.labels.insert("status-text".into(), "Done".into());
    });
    settle(cx).await;
    ensure(
        !view(handle, cx, |view, _, _| view.try_done)?,
        "Try it accepted Done without first seeing a recording",
    )?;
    get().change(|s| {
        s.labels.insert("status-text".into(), "Listening…".into());
    });
    settle(cx).await;
    get().change(|s| {
        s.labels.insert("status-text".into(), "Done".into());
    });
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, _| view.try_listening && view.try_done)?,
        "Try it did not confirm the simulated Listening-to-Done sequence",
    )?;
    passed.push("Try it confirms Listening followed by Done, using simulated core status");

    view(handle, cx, |view, window, cx| {
        view.click("ob-next", window, cx);
        view.click("ob-feature-dictionary", window, cx);
    })?;
    settle(cx).await;
    ensure(
        view(handle, cx, |view, _, _| {
            view.setup().status == Status::Completed && view.tab == "dictionary"
        })?,
        "Opening a feature did not finish setup on the selected tab",
    )?;
    passed.push("Opening a feature completes setup and selects that tab");

    // Let the queued core refresh finish before showing a fixed capture scene.
    settle(cx).await;
    view(handle, cx, |view, window, cx| {
        view.show_scene(long_summary, "subtitling", false, window, cx)
    })?;
    settle(cx).await;
    view(handle, cx, |view, _, cx| {
        view.scroll_to_bottom("subtitling", cx)
    })?;
    settle(cx).await;
    let (offset, max) = view(handle, cx, |view, _, _| {
        view.scroll.get("subtitling").map_or((0., 0.), |h| {
            (f32::from(h.offset().y), f32::from(h.max_offset().height))
        })
    })?;
    ensure(
        max > 0. && (offset + max).abs() < 1.,
        &format!("A long summary did not scroll to its actions (offset {offset}, maximum {max})"),
    )?;
    passed.push("Long summaries scroll the result page so its actions stay reachable");

    reset(handle, cx, "dictionary").await?;
    view(handle, cx, |view, window, cx| {
        view.click("modal-tray", window, cx)
    })?;
    settle(cx).await;
    ensure(
        handle.update(cx, |_, _, _| ()).is_err(),
        "Closing to the tray did not release the window",
    )?;
    let reopened = cx
        .update(|cx| super::window::open_at(cx, point(px(100.), px(100.))))
        .map_err(|e| e.to_string())?
        .ok_or("The window did not reopen")?;
    settle(cx).await;
    ensure(
        view(reopened, cx, |view, _, _| view.tab == "dictionary")?,
        "Reopening from the tray lost the tab",
    )?;
    passed.push("Closing to the tray releases the window and reopening keeps the tab");

    Ok(passed)
}
