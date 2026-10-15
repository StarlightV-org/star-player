//! Opt-in end-to-end checks use their own store, never the user's preferences.
use std::sync::Mutex;

#[test]
#[ignore = "interactive GPUI/native long-video resume lifecycle"]
fn long_video_resume_survives_reopening_and_stop_clears_it() {
    use super::*;
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let clip = temp.path().join("long.avi");
    playback::test_fixtures::avi_timed(&clip, 0, true, 64, 48, 1, 601);
    clip_store::save_config(
        &store,
        &clip_store::Config {
            clip_discovery: clip_store::Discovery::Declined,
            steam_discovery: clip_store::Discovery::Declined,
            ..Default::default()
        },
    )
    .unwrap();
    clip_store::save_history(
        &store,
        &clip_store::History::from([(
            clip_store::hash_filename(&clip).unwrap(),
            clip_store::MediaState {
                identity_version: 2,
                resume_position: Some(123.0),
                ..Default::default()
            },
        )]),
    )
    .unwrap();
    let result = Arc::new(Mutex::new(None));
    let report = result.clone();
    let test_store = store.clone();
    application().with_assets(app_assets::AppAssets).run(move |cx| {
        init(cx);
        let mut player = None;
        let handle = cx.open_window(WindowOptions {
            window_background: WindowBackgroundAppearance::Transparent,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(100.0), px(100.0)), size(px(960.0), px(700.0)),
            ))),
            ..Default::default()
        }, |window, cx| {
            let browser = cx.new(|cx| ClipBrowser::for_test(test_store, cx));
            let shell = cx.new(|cx| PlayerShell::with_browser(browser, window, cx));
            player = Some(shell.clone());
            cx.new(|cx| gpui_kit::base::Root::new(shell, window, cx).bg(rgba(0x00000000)))
        }).unwrap();
        let player = player.unwrap();
        cx.spawn(async move |cx| {
            let mut stage = 0;
            let mut outcome = Err("Resume lifecycle timed out".to_owned());
            for _ in 0..200 {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                let update = handle.update(cx, |_, window, cx| {
                    player.update(cx, |shell, cx| -> Result<bool, String> {
                        match stage {
                            0 if shell.browser.read(cx).audio_ready() => {
                                shell.load_media(clip.clone(), cx);
                                stage = 1;
                            }
                            1 if shell.resume_prompt.is_some() => {
                                if shell.resume_prompt.as_ref().unwrap().position != 123.0
                                    || shell.snapshot.position >= 30.0 {
                                    return Err("Reopening did not offer resume from the saved position".into());
                                }
                                shell.command(Command::Pause, cx);
                                shell.command(Command::SeekAbsolute(321.0), cx);
                                stage = 2;
                            }
                            2 if shell.snapshot.paused && (shell.snapshot.position - 321.0).abs() < 0.01 => {
                                shell.show_library(window, cx);
                                if shell.resume_prompt.is_some() {
                                    return Err("Returning to the library retained the toast".into());
                                }
                                stage = 3;
                            }
                            3 => {
                                if shell.browser.read(cx).media_state(&clip).resume_position != Some(321.0) {
                                    return Err("Returning to the library did not save the position".into());
                                }
                                shell.load_media(clip.clone(), cx);
                                stage = 4;
                            }
                            4 if shell.resume_prompt.is_some() => {
                                if shell.resume_prompt.as_ref().unwrap().position != 321.0 {
                                    return Err("Second reopen lost the checkpoint".into());
                                }
                                shell.command(Command::Stop, cx);
                                stage = 5;
                            }
                            5 if shell.snapshot.paused && shell.snapshot.position == 0.0 => {
                                shell.show_library(window, cx);
                                shell.load_media(clip.clone(), cx);
                                stage = 6;
                            }
                            6 if shell.snapshot.duration > 600.0 => {
                                if shell.resume_prompt.is_some() || shell.resume_candidate.is_some() {
                                    return Err("Stop did not clear the saved resume position".into());
                                }
                                return Ok(true);
                            }
                            _ => {}
                        }
                        Ok(false)
                    })
                });
                match update {
                    Ok(Ok(true)) => { outcome = Ok(()); break; }
                    Ok(Ok(false)) => {}
                    Ok(Err(error)) => { outcome = Err(error); break; }
                    Err(error) => { outcome = Err(error.to_string()); break; }
                }
            }
            *report.lock().unwrap() = Some(outcome);
            let _ = handle.update(cx, |_, _, cx| {
                player.update(cx, |shell, _| shell.shutdown());
                cx.quit();
            });
        }).detach();
        cx.activate(true);
    });
    result
        .lock()
        .unwrap()
        .take()
        .expect("UI test completed")
        .unwrap();
    let state = clip_store::media_state(
        &clip_store::read_history(&store).unwrap(),
        &temp.path().join("long.avi"),
    );
    assert_eq!(state.resume_position, None);
}

#[test]
#[ignore = "interactive GPUI folder history, rename, Escape and fullscreen"]
fn library_history_rename_audio_and_fullscreen() {
    use super::*;
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let root = temp.path().join("Clips");
    let game = root.join("Game");
    let nested = game.join("Nested");
    std::fs::create_dir_all(&nested).unwrap();
    let clip = game.join("clip.avi");
    let renamed = game.join("renamed.avi");
    playback::test_fixtures::avi(&clip, 2, false);
    playback::test_fixtures::avi(&nested.join("nested.avi"), 0, true);
    clip_store::save_config(
        &store,
        &clip_store::Config {
            clip_directories: vec![root],
            clip_discovery: clip_store::Discovery::Declined,
            ..Default::default()
        },
    )
    .unwrap();
    let result = Arc::new(Mutex::new(None));
    let report = result.clone();
    let test_store = store.clone();
    let expected_path = renamed.clone();
    application()
        .with_assets(app_assets::AppAssets)
        .run(move |cx| {
            init(cx);
            let mut player = None;
            let handle = cx
                .open_window(
                    WindowOptions {
                        window_background: WindowBackgroundAppearance::Transparent,
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(100.0), px(100.0)),
                            size(px(960.0), px(700.0)),
                        ))),
                        ..Default::default()
                    },
                    |window, cx| {
                        let browser = cx.new(|cx| ClipBrowser::for_test(test_store, cx));
                        let shell = cx.new(|cx| PlayerShell::with_browser(browser, window, cx));
                        player = Some(shell.clone());
                        cx.new(|cx| {
                            gpui_kit::base::Root::new(shell, window, cx).bg(rgba(0x00000000))
                        })
                    },
                )
                .unwrap();
            let player = player.unwrap();
            cx.spawn(async move |cx| {
                let outcome: Result<(), String> = async {
                    let mut ready = false;
                    for _ in 0..100 {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        ready = handle
                            .update(cx, |_, _, cx| {
                                let browser = player.read(cx).browser.read(cx);
                                browser.audio_ready() && browser.exists(&clip)
                            })
                            .map_err(|e| e.to_string())?;
                        if ready {
                            break;
                        }
                    }
                    if !ready {
                        return Err("Library did not load".into());
                    }
                    handle
                        .update(cx, |_, _, cx| {
                            let browser = player.read(cx).browser.clone();
                            browser.update(cx, |browser, cx| {
                                browser.test_navigate(Some(game.clone()), cx)
                            });
                        })
                        .map_err(|e| e.to_string())?;
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    cx.update_window(handle.into(), |_, window, cx| {
                        window.dispatch_event(
                            MouseDownEvent {
                                position: point(px(400.0), px(300.0)),
                                button: MouseButton::Navigate(NavigationDirection::Back),
                                ..Default::default()
                            }
                            .to_platform_input(),
                            cx,
                        );
                        if player.read(cx).browser.read(cx).test_location().is_some() {
                            return Err("Back did not restore root".into());
                        }
                        window.dispatch_event(
                            MouseDownEvent {
                                position: point(px(400.0), px(300.0)),
                                button: MouseButton::Navigate(NavigationDirection::Forward),
                                ..Default::default()
                            }
                            .to_platform_input(),
                            cx,
                        );
                        if player.read(cx).browser.read(cx).test_location() != Some(game.clone()) {
                            return Err("Forward did not restore folder".into());
                        }
                        let browser = player.read(cx).browser.clone();
                        browser.update(cx, |browser, cx| {
                            browser.test_navigate(Some(nested.clone()), cx)
                        });
                        Ok::<_, String>(())
                    })
                    .map_err(|e| e.to_string())??;
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    cx.update_window(handle.into(), |_, window, cx| {
                        player.update(cx, |shell, cx| shell.navigate_history(false, window, cx));
                        let browser = player.read(cx).browser.clone();
                        if browser.read(cx).test_location() != Some(game.clone()) {
                            return Err("Nested-folder Back failed".into());
                        }
                        browser.update(cx, |browser, cx| browser.test_select(clip.clone(), cx));
                        browser.update(cx, |browser, cx| browser.focus(window, cx));
                        window.dispatch_event(
                            KeyDownEvent {
                                keystroke: Keystroke::parse("escape").unwrap(),
                                is_held: false,
                                prefer_character_input: false,
                            }
                            .to_platform_input(),
                            cx,
                        );
                        if !browser.read(cx).test_selection_empty() {
                            return Err("Escape did not clear selection and anchor".into());
                        }
                        player.update(cx, |shell, cx| shell.load(clip.clone(), cx));
                        Ok::<_, String>(())
                    })
                    .map_err(|e| e.to_string())??;
                    let mut loaded = false;
                    for _ in 0..100 {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        loaded = handle
                            .update(cx, |_, _, cx| player.read(cx).snapshot.tracks.len() == 2)
                            .map_err(|e| e.to_string())?;
                        if loaded {
                            break;
                        }
                    }
                    if !loaded {
                        return Err("Player did not discover tracks".into());
                    }
                    handle
                        .update(cx, |_, _, cx| {
                            player.update(cx, |shell, cx| {
                                shell.command(Command::Pause, cx);
                                shell.command(Command::SeekAbsolute(1.0), cx);
                                shell.command(Command::SetVolume(32.0), cx);
                                shell.command(Command::SetMute(true), cx);
                                shell.command(
                                    Command::SetTrackGain {
                                        id: shell.snapshot.tracks[0].id,
                                        gain: 0.4,
                                    },
                                    cx,
                                );
                                shell.flush_audio(cx);
                            })
                        })
                        .map_err(|e| e.to_string())?;
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    handle
                        .update(cx, |_, _, cx| {
                            let browser = player.read(cx).browser.clone();
                            browser.update(cx, |_, cx| {
                                cx.emit(BrowserEvent::Rename(clip.clone(), "renamed".into()))
                            });
                        })
                        .map_err(|e| e.to_string())?;
                    let mut restored = false;
                    for _ in 0..150 {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        restored = handle
                            .update(cx, |_, _, cx| {
                                let shell = player.read(cx);
                                !shell.rename_busy
                                    && shell.snapshot.path.as_ref() == Some(&expected_path)
                                    && shell.snapshot.tracks.len() == 2
                                    && shell.snapshot.paused
                                    && (shell.snapshot.position - 1.0).abs() < 0.11
                                    && shell.snapshot.tracks[0].gain == 0.4
                                    && shell.snapshot.volume == 32.0
                                    && shell.snapshot.muted
                            })
                            .map_err(|e| e.to_string())?;
                        if restored {
                            break;
                        }
                    }
                    if !restored {
                        return Err(
                            "Rename did not restore path, audio settings and paused position"
                                .into(),
                        );
                    }
                    handle
                        .update(cx, |_, window, _| window.toggle_fullscreen())
                        .map_err(|e| e.to_string())?;
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    handle
                        .update(cx, |_, window, cx| {
                            if !window.is_fullscreen() {
                                return Err("Fullscreen did not activate".into());
                            }
                            player.update(cx, |shell, cx| shell.back_to_library(window, cx));
                            Ok::<_, String>(())
                        })
                        .map_err(|e| e.to_string())??;
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    handle
                        .update(cx, |_, window, cx| {
                            if window.is_fullscreen() {
                                return Err("Returning to library did not exit fullscreen".into());
                            }
                            player
                                .update(cx, |shell, cx| shell.navigate_history(false, window, cx));
                            if player.read(cx).path.as_ref() != Some(&expected_path) {
                                return Err("History still refers to the old filename".into());
                            }
                            Ok::<_, String>(())
                        })
                        .map_err(|e| e.to_string())??;
                    Ok(())
                }
                .await;
                *report.lock().unwrap() = Some(outcome);
                let _ = handle.update(cx, |_, _, cx| {
                    player.update(cx, |shell, cx| {
                        shell.flush_audio(cx);
                        shell.shutdown();
                    });
                    cx.quit();
                });
            })
            .detach();
            cx.activate(true);
        });
    result
        .lock()
        .unwrap()
        .take()
        .expect("UI test completed")
        .unwrap();
    assert!(renamed.exists());
    let config = clip_store::read_config(&store).unwrap();
    assert_eq!(config.volume, 32.0);
    assert!(config.muted);
    let saved = clip_store::media_state(&clip_store::read_history(&store).unwrap(), &renamed);
    assert!(saved.track_gains.values().any(|gain| *gain == 0.4));
}
