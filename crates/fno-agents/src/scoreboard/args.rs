//! Argv parsing for `fno-agents scoreboard`, mirroring the Python leg's
//! typer surface. The two refusals keep exit code 2; their stderr names the
//! harness (the golden freezes typer's usage line), so the parity test
//! checks those two cases on exit code alone.

/// The parsed view request: exactly one fold or the main scoreboard.
#[derive(Debug, Default)]
pub(crate) struct ScoreboardArgs {
    pub since_days: i64,
    pub json_out: ArgFlag,
    pub calibration: ArgFlag,
    pub by_skill: ArgFlag,
    pub efficiency: ArgFlag,
    pub plan_fidelity: ArgFlag,
    pub by_provider: ArgFlag,
    pub lanes: ArgFlag,
    pub project: Option<String>,
}

pub(crate) type ArgFlag = bool;

/// The two refusal exits, with the Python leg's exit code 2.
pub(crate) enum ArgError {
    SinceBelowOne,
    MutuallyExclusive(Vec<&'static str>),
}

pub(crate) fn parse(args: &[String]) -> Result<ScoreboardArgs, ArgError> {
    let mut out = ScoreboardArgs {
        since_days: 28,
        json_out: false,
        calibration: false,
        by_skill: false,
        efficiency: false,
        plan_fidelity: false,
        by_provider: false,
        lanes: false,
        project: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    return Err(ArgError::SinceBelowOne);
                };
                out.since_days = value.parse().map_err(|_| ArgError::SinceBelowOne)?;
            }
            "--json" | "-J" => out.json_out = true,
            "--calibration" => out.calibration = true,
            "--by-skill" => out.by_skill = true,
            "--efficiency" => out.efficiency = true,
            "--plan-fidelity" => out.plan_fidelity = true,
            "--by-provider" => out.by_provider = true,
            "--lanes" => out.lanes = true,
            "--project" => {
                i += 1;
                out.project = args.get(i).cloned();
            }
            _ => {}
        }
        i += 1;
    }
    // The mutually-exclusive view check runs after parsing, the same place
    // the Python leg raised its BadParameter.
    let views: Vec<&str> = [
        ("--calibration", out.calibration),
        ("--by-skill", out.by_skill),
        ("--efficiency", out.efficiency),
        ("--plan-fidelity", out.plan_fidelity),
        ("--by-provider", out.by_provider),
        ("--lanes", out.lanes),
    ]
    .iter()
    .filter(|(_, on)| *on)
    .map(|(name, _)| *name)
    .collect();
    if views.len() > 1 {
        return Err(ArgError::MutuallyExclusive(views));
    }
    if out.since_days < 1 {
        return Err(ArgError::SinceBelowOne);
    }
    Ok(out)
}
