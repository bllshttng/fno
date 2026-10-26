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
    pub help: ArgFlag,
}

pub(crate) type ArgFlag = bool;

/// The refusal exits, with the Python leg's typer exit code 2. The Python
/// leg refused through typer: help printed usage at exit 0, a misspelled
/// flag or stray positional refused, and a value-less option refused.
pub(crate) enum ArgError {
    SinceBelowOne,
    MutuallyExclusive(Vec<&'static str>),
    Unknown(String),
    MissingValue(&'static str),
}

/// The usage line `--help` prints (the typer help's own command line).
pub(crate) const USAGE: &str = "usage: fno-agents scoreboard [--since <days>] [--json] [--project <slug>]\n       or one view: --calibration | --by-skill | --efficiency | --plan-fidelity | --by-provider | --lanes";

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
        help: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                out.help = true;
                return Ok(out);
            }
            "--since" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    return Err(ArgError::MissingValue("--since"));
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
                out.project = Some(
                    args.get(i)
                        .filter(|v| !v.starts_with('-'))
                        .ok_or(ArgError::MissingValue("--project"))?
                        .clone(),
                );
            }
            other => return Err(ArgError::Unknown(other.to_string())),
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
