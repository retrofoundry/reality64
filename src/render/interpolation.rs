use ultragfx::RefreshRate;

pub(super) struct Settings {
    override_rate: Option<RefreshRate>,
    capture: bool,
}

fn parse(value: &str) -> anyhow::Result<RefreshRate> {
    let rate = match value {
        "original" => RefreshRate::Original,
        "display" => RefreshRate::Display,
        _ => {
            let hz = value.strip_prefix("manual:")
                .ok_or_else(|| anyhow::anyhow!("invalid REALITY64_REFRESH_RATE {value:?}; expected original, display, or manual:<hz>"))?
                .parse::<f64>()?;
            RefreshRate::Manual(hz)
        }
    };
    rate.validate()
        .map_err(|error| anyhow::anyhow!("invalid REALITY64_REFRESH_RATE {value:?}: {error:?}"))?;
    Ok(rate)
}

impl Settings {
    pub fn from_env(capture: bool) -> anyhow::Result<Self> {
        let override_rate = match std::env::var("REALITY64_REFRESH_RATE") {
            Ok(value) => Some(parse(&value)?),
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            override_rate,
            capture,
        })
    }

    pub fn rate(&self, port: RefreshRate) -> RefreshRate {
        if self.capture {
            RefreshRate::Original
        } else {
            self.override_rate.unwrap_or(port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_parses_arbitrary_valid_rates_and_rejects_invalid_input() {
        for (text, expected) in [
            ("original", RefreshRate::Original),
            ("display", RefreshRate::Display),
            ("manual:59.94", RefreshRate::Manual(59.94)),
        ] {
            assert_eq!(parse(text).unwrap(), expected);
        }
        for text in [
            "",
            "fixed60",
            "manual:0",
            "manual:-1",
            "manual:NaN",
            "manual:inf",
            "manual:",
            "display:60",
            " original",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn override_wins_over_live_port_changes_and_capture_forces_original() {
        let mut settings = Settings {
            override_rate: None,
            capture: false,
        };
        assert_eq!(settings.rate(RefreshRate::Display), RefreshRate::Display);
        settings.override_rate = Some(RefreshRate::Manual(60.0));
        assert_eq!(
            settings.rate(RefreshRate::Original),
            RefreshRate::Manual(60.0)
        );
        settings.capture = true;
        assert_eq!(settings.rate(RefreshRate::Display), RefreshRate::Original);
    }
}
