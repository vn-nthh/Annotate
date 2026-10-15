//! `--capture-ui <dir>`: renders every screen with fixture data and saves PNGs
//! named like the Win32 reference captures, for visual comparison. Uses an
//! isolated profile and never reads credentials or contacts APIs.
use super::window::MainWindow;
use crate::native_core::{get, State};
use crate::native_onboarding::{self as setup, Onboarding};
use gpui::{point, px, App, AsyncApp, WindowHandle};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Scene {
    name: String,
    tab: &'static str,
    modal: bool,
    state: State,
    bottom: Option<&'static str>,
}

fn fixture(state: &mut State, value: Value) {
    if let Some(file) = value["file"].as_str() {
        state.file = Some(file.into());
    }
    if let Some(entries) = value.get("entries") {
        state.entries = serde_json::from_value(entries.clone()).unwrap_or_default();
    }
    if let Some(summary) = value["summary"].as_str() {
        state.summary = summary.into();
    }
    if value["openrouter"].as_bool() == Some(true) {
        state
            .labels
            .insert("openrouter-key-input".into(), "UI fixture only".into());
    }
    if let Some(ready) = value["ready"].as_array() {
        for name in ready.iter().filter_map(|v| v.as_str()) {
            state.ready.insert(name.into());
        }
    }
    let time = 1_700_000_000_000;
    if let Some(terms) = value["dictionary"].as_array() {
        state.data.data.dictionary.terms = terms
            .iter()
            .filter_map(|v| v.as_str())
            .map(|value| crate::native_data::Term {
                value: value.into(),
                updated_at: time,
            })
            .collect();
    }
    if let Some(entries) = value["home"].as_array() {
        state.data.data.history.entries = entries
            .iter()
            .filter_map(|v| v.as_str())
            .map(|text| crate::native_data::Entry {
                text: text.into(),
                time,
                updated_at: time,
            })
            .collect();
    }
}

fn scenes(initial: &State) -> Vec<Scene> {
    let entries = json!([
        {"index": 1, "start": 0.0, "end": 2.5, "text": "Welcome to Annotate."},
        {"index": 2, "start": 3.0, "end": 5.5, "text": "This is a native Windows transcription app."},
        {"index": 3, "start": 6.0, "end": 8.5, "text": "Vulkan runs the local model on your GPU."}
    ]);
    let plain: Vec<(&str, &'static str, Value)> = vec![
        ("home", "home", json!({})),
        (
            "home-filled",
            "home",
            json!({"home": [
                "A short transcription.",
                "This longer transcription demonstrates wrapping across several lines while retaining the original card padding and spacing.",
                "Native recording stays available when the settings window is closed."
            ]}),
        ),
        ("dictionary", "dictionary", json!({})),
        (
            "dictionary-filled",
            "dictionary",
            json!({"dictionary": ["Annotate", "résumé", "Vulkan"]}),
        ),
        ("subtitling", "subtitling", json!({})),
        (
            "subtitling-file",
            "subtitling",
            json!({"file": "example.wav", "ready": []}),
        ),
        (
            "subtitling-ready",
            "subtitling",
            json!({"file": "example.wav", "ready": ["ffmpeg", "vad"]}),
        ),
        (
            "subtitling-result",
            "subtitling",
            json!({"file": "example.wav", "entries": entries, "summary": "", "openrouter": true}),
        ),
        (
            "subtitling-summary",
            "subtitling",
            json!({"file": "example.wav", "entries": entries, "openrouter": true,
                "summary": "Annotate transcribes speech using a native Windows interface. It supports cloud services and a local Vulkan model."}),
        ),
        ("modal", "home", json!({})),
    ];
    let mut scenes = vec![];
    for theme in ["light", "dark"] {
        let mut base = initial.clone();
        base.mics = vec![crate::AudioDevice {
            id: String::new(),
            name: "System Default".into(),
        }];
        base.settings.insert("theme".into(), json!(theme));
        for (name, tab, value) in &plain {
            let mut state = base.clone();
            fixture(&mut state, value.clone());
            scenes.push(Scene {
                name: format!("{theme}-{name}"),
                tab,
                modal: *name == "modal",
                state,
                bottom: None,
            });
        }
        let mut state = base.clone();
        fixture(
            &mut state,
            json!({"file": "example.wav", "entries": entries, "openrouter": true,
                "summary": "A long summary must not push the actions out of reach. ".repeat(14)}),
        );
        scenes.push(Scene {
            name: format!("{theme}-subtitling-long-summary"),
            tab: "subtitling",
            modal: false,
            state,
            bottom: Some("subtitling"),
        });
        for mode in ["azure-mai", "groq", "local"] {
            for grammar in ["0", "1"] {
                for user in ["in", "out", "waiting"] {
                    let mut state = base.clone();
                    state
                        .settings
                        .insert("transcriptionMode".into(), json!(mode));
                    state
                        .settings
                        .insert("grammarCleanupEnabled".into(), json!(grammar == "1"));
                    if user == "in" {
                        state.user = Some(
                            json!({"name": "Example User", "email": "example@example.com", "picture": "fixture"}),
                        );
                        state.avatar = Some(Arc::new(
                            include_bytes!("../../assets/profile-avatar-fixture.png").to_vec(),
                        ));
                    }
                    state.auth_waiting = user == "waiting";
                    let bottom = mode == "local" && grammar == "1" && user == "in";
                    scenes.push(Scene {
                        name: format!("{theme}-settings-{mode}-vulkan-{grammar}-{user}"),
                        tab: "settings",
                        modal: false,
                        state: state.clone(),
                        bottom: None,
                    });
                    if bottom {
                        scenes.push(Scene {
                            name: format!("{theme}-settings-bottom"),
                            tab: "settings",
                            modal: false,
                            state,
                            bottom: Some("settings"),
                        });
                    }
                }
            }
        }
        for (index, (step, _)) in setup::STEPS.iter().enumerate() {
            let engines: &[&str] = if matches!(*step, "model" | "install") {
                &setup::ENGINES
            } else {
                &["groq"]
            };
            for engine in engines {
                let mut state = base.clone();
                let mut guide = Onboarding::initial(false, engine);
                guide.step = index;
                state.settings.insert(setup::KEY.into(), guide.to_value());
                if *step == "try" {
                    state
                        .settings
                        .insert("hotkey".into(), json!("Ctrl+Shift+Space"));
                }
                scenes.push(Scene {
                    name: format!("{theme}-onboarding-{step}-{engine}"),
                    tab: "home",
                    modal: false,
                    state,
                    bottom: None,
                });
            }
        }
    }
    scenes
}

pub fn run(directory: PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    super::run_with(move |cx: &mut App| {
        crate::native_core::initialize(crate::app::AppHandle)?;
        get().set_setting(setup::KEY, Onboarding::initial(true, "groq").to_value())?;
        let handle = super::window::open_at(cx, point(px(100.), px(100.)))
            .ok_or("Cannot open the capture window")?;
        cx.spawn(async move |cx| {
            if let Err(error) = capture_all(handle, &directory, cx).await {
                eprintln!("Native UI capture: {error}");
            }
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
        Ok(())
    })
}

async fn capture_all(
    handle: WindowHandle<MainWindow>,
    directory: &std::path::Path,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let initial = get().snapshot();
    for scene in scenes(&initial) {
        let path = directory.join(format!("{}.png", scene.name));
        handle
            .update(cx, |view, window, cx| {
                view.show_scene(scene.state, scene.tab, scene.modal, window, cx)
            })
            .map_err(|e| e.to_string())?;
        settle(cx).await;
        if let Some(id) = scene.bottom {
            handle
                .update(cx, |view, _, cx| view.scroll_to_bottom(id, cx))
                .map_err(|e| e.to_string())?;
            settle(cx).await;
        }
        handle
            .update(cx, |_, window, _| {
                crate::platform::capture_window(window, &path)
            })
            .map_err(|e| e.to_string())??;
    }
    Ok(())
}

/// Two frames: one to lay out (scroll handles learn their size), one to paint.
async fn settle(cx: &mut AsyncApp) {
    cx.background_executor()
        .timer(Duration::from_millis(250))
        .await;
}
