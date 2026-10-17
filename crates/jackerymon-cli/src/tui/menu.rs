//! The settings menu: one list; ←/→ cycles the selected row's value,
//! Enter applies it, and a risky value asks y/n inside the same box.

use crossterm::event::KeyCode;
use jackerymon::control::OFF_TIMER_CHOICES;
use jackerymon::control::Setting;
use jackerymon::identity::ModelCode;
use jackerymon::status::BatteryProtection;
use jackerymon::status::ChargeSpeed;
use jackerymon::status::LightMode;
use jackerymon::status::ScreenMode;
use jackerymon::status::Status;

/// One menu row: a control and the values it can take.
pub(super) struct Control {
    pub(super) label: &'static str,
    pub(super) choices: Vec<Setting>,
}

impl Control {
    /// The choice `status` shows in effect.
    pub(super) fn current(&self, status: Option<&Status>) -> Option<usize> {
        let status = status?;
        self.choices
            .iter()
            .position(|choice| choice.applied(status))
    }
}

/// The controls for a unit of `model`, all of them when it is not known.
pub(super) fn controls(model: Option<ModelCode>) -> Vec<Control> {
    let switch = |setting: fn(bool) -> Setting| vec![setting(true), setting(false)];
    let all = vec![
        Control {
            label: "AC output",
            choices: switch(Setting::AcOutput),
        },
        Control {
            label: "DC output",
            choices: switch(Setting::DcOutput),
        },
        Control {
            label: "Light",
            choices: [
                LightMode::Off,
                LightMode::Low,
                LightMode::High,
                LightMode::Sos,
            ]
            .map(Setting::Light)
            .to_vec(),
        },
        Control {
            label: "Screen",
            choices: [ScreenMode::Off, ScreenMode::Brief, ScreenMode::Steady]
                .map(Setting::Screen)
                .to_vec(),
        },
        Control {
            label: "Auto-off",
            choices: OFF_TIMER_CHOICES.map(Setting::AutoOff).to_vec(),
        },
        Control {
            label: "Energy saving",
            choices: OFF_TIMER_CHOICES.map(Setting::EnergySaving).to_vec(),
        },
        Control {
            label: "Charge speed",
            choices: [ChargeSpeed::Fast, ChargeSpeed::Quiet]
                .map(Setting::ChargeSpeed)
                .to_vec(),
        },
        Control {
            label: "Battery protection",
            choices: [BatteryProtection::Full, BatteryProtection::Eco]
                .map(Setting::BatteryProtection)
                .to_vec(),
        },
        Control {
            label: "Emergency charge",
            choices: switch(Setting::EmergencyCharge),
        },
        Control {
            label: "UPS",
            choices: switch(Setting::Ups),
        },
    ];
    // A control is left out when none of its choices is on the model.
    all.into_iter()
        .filter(|control| {
            model.is_none_or(|model| {
                control
                    .choices
                    .iter()
                    .any(|choice| choice.available_on(model))
            })
        })
        .collect()
}

/// Where the user is in the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Menu {
    Closed,
    /// Row `row` selected; `choice` is a value picked with ←/→ but not
    /// yet applied.
    Open {
        row: usize,
        choice: Option<usize>,
    },
    /// Asking before applying a setting with a warning.
    Confirm {
        row: usize,
        choice: usize,
    },
}

impl Menu {
    /// Applies one key press; returns a setting to send, if chosen.
    pub(super) fn handle(
        &mut self,
        key: KeyCode,
        controls: &[Control],
        status: Option<&Status>,
    ) -> Option<Setting> {
        let shown =
            |row: usize, choice: Option<usize>| choice.or_else(|| controls[row].current(status));
        let (next, chosen) = match (*self, key) {
            (Self::Closed, KeyCode::Char('m')) => (
                Self::Open {
                    row: 0,
                    choice: None,
                },
                None,
            ),
            // Moving discards a value that was not applied.
            (Self::Open { row, .. }, KeyCode::Up) => {
                let row = previous(row, controls.len());
                (Self::Open { row, choice: None }, None)
            }
            (Self::Open { row, .. }, KeyCode::Down) => {
                let row = next(row, controls.len());
                (Self::Open { row, choice: None }, None)
            }
            (Self::Open { row, choice }, KeyCode::Left | KeyCode::Right) => {
                let count = controls[row].choices.len();
                let choice = match (shown(row, choice), key) {
                    (Some(index), KeyCode::Left) => previous(index, count),
                    (Some(index), _) => next(index, count),
                    (None, _) => 0,
                };
                (
                    Self::Open {
                        row,
                        choice: Some(choice),
                    },
                    None,
                )
            }
            (
                Self::Open {
                    row,
                    choice: Some(choice),
                },
                KeyCode::Enter,
            ) => {
                let setting = controls[row].choices[choice];
                if setting.warning().is_some() {
                    (Self::Confirm { row, choice }, None)
                } else {
                    (Self::Open { row, choice: None }, Some(setting))
                }
            }
            (
                Self::Open {
                    row,
                    choice: Some(_),
                },
                KeyCode::Esc,
            ) => (Self::Open { row, choice: None }, None),
            (Self::Open { choice: None, .. }, KeyCode::Esc | KeyCode::Char('m')) => {
                (Self::Closed, None)
            }
            (Self::Confirm { row, choice }, KeyCode::Char('y')) => (
                Self::Open { row, choice: None },
                Some(controls[row].choices[choice]),
            ),
            (Self::Confirm { row, choice }, KeyCode::Char('n') | KeyCode::Esc) => (
                Self::Open {
                    row,
                    choice: Some(choice),
                },
                None,
            ),
            (unchanged, _) => (unchanged, None),
        };
        *self = next;
        chosen
    }
}

fn previous(index: usize, count: usize) -> usize {
    (index + count - 1) % count
}

fn next(index: usize, count: usize) -> usize {
    (index + 1) % count
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use jackerymon::control::Setting;
    use jackerymon::identity::ModelCode;
    use jackerymon::status::LightMode;
    use jackerymon::status::Status;
    use serde_json::json;

    use super::Menu;
    use super::controls;

    fn press(menu: &mut Menu, keys: &[KeyCode], status: Option<&Status>) -> Option<Setting> {
        let controls = controls(None);
        keys.iter()
            .filter_map(|key| menu.handle(*key, &controls, status))
            .last()
    }

    fn row_of(setting: Setting) -> usize {
        controls(None)
            .iter()
            .position(|control| control.choices.contains(&setting))
            .expect("row")
    }

    #[test]
    fn a_model_without_a_light_gets_no_light_control() {
        let labels = |model| {
            controls(model)
                .iter()
                .map(|control| control.label)
                .collect::<Vec<_>>()
        };
        assert!(labels(Some(ModelCode::EXPLORER_2000)).contains(&"Light"));
        assert!(!labels(Some(ModelCode::EXPLORER_3000)).contains(&"Light"));
        assert_eq!(labels(None), labels(Some(ModelCode::EXPLORER_2000)));
    }

    #[test]
    fn arrows_cycle_from_current_value_and_enter_applies() {
        let status = Status::try_from(json!({"lm": 1}).as_object().expect("object").clone())
            .expect("status");
        let mut menu = Menu::Closed;
        let keys = [
            KeyCode::Char('m'),
            KeyCode::Down,
            KeyCode::Down,
            KeyCode::Right,
        ];
        assert_eq!(press(&mut menu, &keys, Some(&status)), None);
        assert_eq!(
            menu,
            Menu::Open {
                row: 2,
                choice: Some(2)
            }
        );
        let chosen = press(&mut menu, &[KeyCode::Enter], Some(&status));
        assert_eq!(chosen, Some(Setting::Light(LightMode::High)));
        assert_eq!(
            menu,
            Menu::Open {
                row: 2,
                choice: None
            },
            "menu stays open"
        );
    }

    #[test]
    fn outputs_apply_without_confirmation() {
        let mut menu = Menu::Closed;
        let keys = [KeyCode::Char('m'), KeyCode::Right, KeyCode::Enter];
        assert_eq!(press(&mut menu, &keys, None), Some(Setting::AcOutput(true)));
    }

    #[test]
    fn emergency_charge_needs_confirmation() {
        let row = row_of(Setting::EmergencyCharge(true));
        let mut menu = Menu::Open {
            row,
            choice: Some(0),
        };
        assert_eq!(press(&mut menu, &[KeyCode::Enter], None), None);
        assert_eq!(menu, Menu::Confirm { row, choice: 0 });
        assert_eq!(press(&mut menu, &[KeyCode::Char('n')], None), None);
        assert_eq!(
            menu,
            Menu::Open {
                row,
                choice: Some(0)
            }
        );
        let chosen = press(&mut menu, &[KeyCode::Enter, KeyCode::Char('y')], None);
        assert_eq!(chosen, Some(Setting::EmergencyCharge(true)));
    }

    #[test]
    fn escape_discards_then_closes_and_moving_discards() {
        let mut menu = Menu::Closed;
        press(&mut menu, &[KeyCode::Char('m'), KeyCode::Up], None);
        assert_eq!(
            menu,
            Menu::Open {
                row: controls(None).len() - 1,
                choice: None
            }
        );
        press(&mut menu, &[KeyCode::Right, KeyCode::Down], None);
        assert_eq!(
            menu,
            Menu::Open {
                row: 0,
                choice: None
            }
        );
        press(&mut menu, &[KeyCode::Right, KeyCode::Esc], None);
        assert_eq!(
            menu,
            Menu::Open {
                row: 0,
                choice: None
            }
        );
        press(&mut menu, &[KeyCode::Esc], None);
        assert_eq!(menu, Menu::Closed);
    }
}
