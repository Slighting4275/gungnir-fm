//! Exercise real Slint key dispatch without a display or controller side effects.

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PlatformError, WindowAdapter, WindowEvent};

use crate::controller::{Command, ContextAction, Msg};

struct HeadlessPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn press(ui: &crate::AppWindow, text: impl Into<slint::SharedString>) {
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.into() });
}

fn release(ui: &crate::AppWindow, text: impl Into<slint::SharedString>) {
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

#[test]
fn keyboard_file_commands_use_the_displayed_row_revision() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform(window.clone())))?;
    window.set_size(slint::PhysicalSize::new(1320, 820));
    let ui = crate::AppWindow::new()?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    crate::wire(&ui, &tx);
    ui.set_left_rows_revision(41);
    ui.show()?;
    ui.window().dispatch_event(WindowEvent::WindowActiveChanged(true));
    ui.invoke_focus_files();

    for (key, expected) in [(Key::Return, "open"), (Key::Delete, "trash")] {
        press(&ui, key);
        release(&ui, key);
        let commands: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(commands.iter().any(|msg| matches!(msg,
            Msg::UiRows { panes: [Some(41), None], command: Command::RunInPane { pane: 0, name } } if name == expected
        )), "{commands:?}");
        assert!(!commands.iter().any(|msg| matches!(msg, Msg::Ui(Command::Run(name)) if name == expected)));
    }
    ui.hide()?;
    Ok(())
}

#[test]
fn popup_commands_keep_the_selection_presented_when_opened() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform(window)))?;
    let ui = crate::AppWindow::new()?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    crate::wire(&ui, &tx);
    let model = Rc::new(slint::VecModel::from(vec![
        crate::FileRow { uri: "file:///a".into(), selected: true, ..Default::default() },
        crate::FileRow { uri: "file:///b".into(), ..Default::default() },
    ]));
    ui.set_left_files(slint::ModelRc::from(model.clone()));
    assert_eq!(ui.invoke_context_target_count(0, 0, 17), 1);

    slint::Model::set_row_data(
        &*model,
        1,
        crate::FileRow { uri: "file:///b".into(), selected: true, ..Default::default() },
    );
    ui.invoke_context_command(0, 17, "delete".into());
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| matches!(msg,
        Msg::UiRows { panes: [Some(17), None], command: Command::RunContextAction {
            pane: 0, action: ContextAction::BuiltIn(name), targets: Some(targets)
        } } if name == "delete" && targets == ["file:///a"]
    )));
    Ok(())
}

#[test]
fn modifier_keys_do_not_trigger_letter_shortcuts() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform(window.clone())))?;
    window.set_size(slint::PhysicalSize::new(1320, 820));
    let ui = crate::AppWindow::new()?;
    let actions = Rc::new(RefCell::new(Vec::<String>::new()));
    ui.on_command({
        let actions = actions.clone();
        move |command| actions.borrow_mut().push(command.to_string())
    });
    ui.on_nav({
        let actions = actions.clone();
        move |_, action| actions.borrow_mut().push(action.to_string())
    });
    ui.on_tab_new({
        let actions = actions.clone();
        move |_| actions.borrow_mut().push("new-tab".into())
    });
    ui.on_tab_close({
        let actions = actions.clone();
        move |_, _| actions.borrow_mut().push("close-tab".into())
    });
    ui.on_filter_changed({
        let actions = actions.clone();
        move |_, text| actions.borrow_mut().push(format!("filter:{text}"))
    });
    ui.on_navigate({
        let actions = actions.clone();
        move |_, location| actions.borrow_mut().push(format!("navigate:{location}"))
    });
    ui.show()?;
    ui.window().dispatch_event(WindowEvent::WindowActiveChanged(true));
    let mut pixels = vec![slint::Rgb8Pixel::default(); 1320 * 820];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, 1320);
    });

    for control in [Key::Control, Key::ControlR] {
        ui.invoke_focus_files();
        press(&ui, control);
        release(&ui, control);
        assert!(actions.borrow().is_empty(), "{control:?} alone triggered {:?}", actions.borrow());

        // These Slint key codes overlap ASCII Ctrl+R/T/W. They must never
        // refresh, open a tab, or close a tab merely because Ctrl is held.
        for modifier in [Key::Shift, Key::ShiftR, Key::Alt, Key::AltGr, Key::CapsLock, Key::Meta, Key::MetaR] {
            ui.invoke_focus_files();
            press(&ui, control);
            press(&ui, modifier);
            release(&ui, modifier);
            release(&ui, control);
            assert!(actions.borrow().is_empty(), "{control:?}+{modifier:?} triggered {:?}", actions.borrow());
        }

        for (letter, expected) in [
            ("q", "quit"),
            ("Q", "quit"),
            ("r", "refresh"),
            ("h", "toggle-hidden"),
            ("t", "new-tab"),
            ("w", "close-tab"),
            ("c", "copy"),
        ] {
            ui.invoke_focus_files();
            press(&ui, control);
            press(&ui, letter);
            release(&ui, letter);
            release(&ui, control);
            assert_eq!(&*actions.borrow(), &[expected], "{control:?}+{letter}");
            actions.borrow_mut().clear();
        }

        ui.invoke_focus_files();
        press(&ui, control);
        press(&ui, Key::Backspace);
        release(&ui, Key::Backspace);
        release(&ui, control);
        assert_eq!(&*actions.borrow(), &["up"], "Ctrl+Backspace must not act as Ctrl+H");
        actions.borrow_mut().clear();

        ui.invoke_focus_files();
        press(&ui, control);
        press(&ui, "f");
        release(&ui, "f");
        release(&ui, control);
        press(&ui, "x");
        release(&ui, "x");
        assert_eq!(actions.borrow().last().map(String::as_str), Some("filter:x"), "Ctrl+F must focus the filter");
        actions.borrow_mut().clear();

        ui.invoke_focus_files();
        press(&ui, control);
        press(&ui, "l");
        release(&ui, "l");
        release(&ui, control);
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 1320);
        });
        for character in "/keyboard-test".chars() {
            let text = character.to_string();
            press(&ui, text.as_str());
            release(&ui, text.as_str());
        }
        press(&ui, Key::Return);
        release(&ui, Key::Return);
        assert_eq!(&*actions.borrow(), &["navigate:/keyboard-test"], "Ctrl+L must focus the location field");
        actions.borrow_mut().clear();
    }
    ui.hide()?;
    Ok(())
}
