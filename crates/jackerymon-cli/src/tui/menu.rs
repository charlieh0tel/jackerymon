//! The settings menu: pick a control, pick a value, confirm if it cuts
//! power.

use crossterm::event::KeyCode;
use jackerymon::control::OFF_TIMER_CHOICES;
use jackerymon::control::Setting;
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

pub(super) fn controls() -> Vec<Control> {
    let switch = |setting: fn(bool) -> Setting| vec![setting(true), setting(false)];
    vec![
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
    ]
}

/// Where the user is in the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Menu {
    Closed,
    /// Choosing a control.
    Controls {
        row: usize,
    },
    /// Choosing a value for control `row`.
    Choices {
        row: usize,
        choice: usize,
    },
    /// Asking before a setting with a warning.
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
        let (next, chosen) = match (*self, key) {
            (Self::Closed, KeyCode::Char('m')) => (Self::Controls { row: 0 }, None),
            (Self::Controls { row }, KeyCode::Up) => (
                Self::Controls {
                    row: previous(row, controls.len()),
                },
                None,
            ),
            (Self::Controls { row }, KeyCode::Down) => (
                Self::Controls {
                    row: next(row, controls.len()),
                },
                None,
            ),
            (Self::Controls { row }, KeyCode::Enter | KeyCode::Right) => {
                let choice = controls[row].current(status).unwrap_or(0);
                (Self::Choices { row, choice }, None)
            }
            (Self::Controls { .. }, KeyCode::Esc | KeyCode::Left | KeyCode::Char('m')) => {
                (Self::Closed, None)
            }
            (Self::Choices { row, choice }, KeyCode::Up) => {
                let count = controls[row].choices.len();
                (
                    Self::Choices {
                        row,
                        choice: previous(choice, count),
                    },
                    None,
                )
            }
            (Self::Choices { row, choice }, KeyCode::Down) => {
                let count = controls[row].choices.len();
                (
                    Self::Choices {
                        row,
                        choice: next(choice, count),
                    },
                    None,
                )
            }
            (Self::Choices { row, choice }, KeyCode::Enter) => {
                let setting = controls[row].choices[choice];
                if setting.warning().is_some() {
                    (Self::Confirm { row, choice }, None)
                } else {
                    (Self::Closed, Some(setting))
                }
            }
            (Self::Choices { row, .. }, KeyCode::Esc | KeyCode::Left) => {
                (Self::Controls { row }, None)
            }
            (Self::Confirm { row, choice }, KeyCode::Char('y')) => {
                (Self::Closed, Some(controls[row].choices[choice]))
            }
            (Self::Confirm { row, choice }, KeyCode::Char('n') | KeyCode::Esc) => {
                (Self::Choices { row, choice }, None)
            }
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
    use jackerymon::status::LightMode;
    use jackerymon::status::Status;
    use serde_json::json;

    use super::Menu;
    use super::controls;

    fn press(menu: &mut Menu, keys: &[KeyCode], status: Option<&Status>) -> Option<Setting> {
        let controls = controls();
        keys.iter()
            .filter_map(|key| menu.handle(*key, &controls, status))
            .last()
    }

    #[test]
    fn choosing_light_starts_at_current_value() {
        let status = Status::try_from(json!({"lm": 1}).as_object().expect("object").clone())
            .expect("status");
        let mut menu = Menu::Closed;
        let chosen = press(
            &mut menu,
            &[
                KeyCode::Char('m'),
                KeyCode::Down,
                KeyCode::Down,
                KeyCode::Enter,
                KeyCode::Down,
                KeyCode::Enter,
            ],
            Some(&status),
        );
        assert_eq!(chosen, Some(Setting::Light(LightMode::High)));
        assert_eq!(menu, Menu::Closed);
    }

    #[test]
    fn outputs_apply_without_confirmation() {
        let mut menu = Menu::Closed;
        let keys = [
            KeyCode::Char('m'),
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Enter,
        ];
        assert_eq!(
            press(&mut menu, &keys, None),
            Some(Setting::AcOutput(false))
        );
        assert_eq!(menu, Menu::Closed);
    }

    #[test]
    fn emergency_charge_needs_confirmation() {
        let row = controls()
            .iter()
            .position(|control| control.choices.contains(&Setting::EmergencyCharge(true)))
            .expect("emergency charge row");
        let mut menu = Menu::Choices { row, choice: 0 };
        assert_eq!(press(&mut menu, &[KeyCode::Enter], None), None);
        assert_eq!(menu, Menu::Confirm { row, choice: 0 });
        assert_eq!(press(&mut menu, &[KeyCode::Char('n')], None), None);
        assert_eq!(menu, Menu::Choices { row, choice: 0 });
        let chosen = press(&mut menu, &[KeyCode::Enter, KeyCode::Char('y')], None);
        assert_eq!(chosen, Some(Setting::EmergencyCharge(true)));
    }

    #[test]
    fn escape_backs_out_and_rows_wrap() {
        let mut menu = Menu::Closed;
        press(&mut menu, &[KeyCode::Char('m'), KeyCode::Up], None);
        assert_eq!(
            menu,
            Menu::Controls {
                row: controls().len() - 1
            }
        );
        press(
            &mut menu,
            &[KeyCode::Enter, KeyCode::Esc, KeyCode::Esc],
            None,
        );
        assert_eq!(menu, Menu::Closed);
    }
}
