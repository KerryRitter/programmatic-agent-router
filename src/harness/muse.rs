use super::{
    add_passthrough, add_yolo_args, is_json_output, plain_model, resume_is_latest, Harness,
    Invocation, Request,
};

pub(crate) fn new() -> Box<dyn Harness> {
    Box::new(MuseHarness)
}

struct MuseHarness;

impl Harness for MuseHarness {
    fn build(&self, request: &Request) -> Result<Invocation, String> {
        let mut args = Vec::new();

        if request.prompt.is_some() {
            // Headless runs go through `muse exec [OPTIONS] <PROMPT>`. A bare
            // `muse "<prompt>"` seeds the interactive TUI instead, which hangs
            // under capture.
            args.push("exec".to_string());
        } else if let Some(resume) = &request.resume_id {
            // `muse resume` is interactive-only (session picker / `--last`), so
            // it is reachable only on the no-prompt path.
            args.push("resume".to_string());
            if resume_is_latest(resume) {
                args.push("--last".to_string());
            } else {
                args.push(resume.clone());
            }
        }

        if let Some(model) = plain_model(request) {
            args.extend(["--model".to_string(), model]);
        }

        if request.prompt.is_some() {
            // `--json` emits JSONL events; muse has no other machine format.
            if is_json_output(request) {
                args.push("--json".to_string());
            }
            // muse caps work by model steps rather than conversational turns.
            if let Some(max_turns) = &request.max_turns {
                args.extend(["--max-model-steps".to_string(), max_turns.clone()]);
            }
            // `muse exec` has no resume flag, but reusing a session id appends to
            // that session's event log — so it both sets and resumes. Resume wins
            // when both are given. "latest"/"last" has no headless equivalent.
            match (&request.resume_id, &request.session_id) {
                (Some(resume), _) => {
                    if resume_is_latest(resume) {
                        return Err(
                            "muse cannot resume the latest session headlessly; pass the session uuid to --resume-id, or drop the prompt to use `muse resume --last`"
                                .to_string(),
                        );
                    }
                    args.extend(["--session-id".to_string(), resume.clone()]);
                }
                (None, Some(session)) => {
                    args.extend(["--session-id".to_string(), session.clone()]);
                }
                (None, None) => {}
            }
        } else if let Some(mode) = &request.permission_mode {
            // `--approval-mode untrusted|on-request|never` is a root-only flag;
            // `muse exec` rejects it outright, so it applies interactively only.
            args.extend(["--approval-mode".to_string(), mode.clone()]);
        }

        let mut args = add_yolo_args(args, request)?;

        if let Some(prompt) = &request.prompt {
            args.push(prompt.clone());
        }

        Ok(Invocation::new("muse", add_passthrough(args, request)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CliOptions;

    fn build(options: CliOptions) -> Result<Invocation, String> {
        let request = Request::from_options(options, String::new()).unwrap();
        MuseHarness.build(&request)
    }

    #[test]
    fn prompt_runs_headless_exec_with_prompt_last() {
        let invocation = build(CliOptions {
            harness: "muse".to_string(),
            model: Some("muse-spark-1.2".to_string()),
            output_format: Some("json".to_string()),
            max_turns: Some("12".to_string()),
            prompt: Some("fix the build".to_string()),
            yolo: true,
            ..CliOptions::default()
        })
        .unwrap();

        assert_eq!(invocation.command, "muse");
        assert_eq!(
            invocation.args,
            vec![
                "exec",
                "--model",
                "muse-spark-1.2",
                "--json",
                "--max-model-steps",
                "12",
                "--yolo",
                "fix the build",
            ]
        );
    }

    #[test]
    fn no_prompt_stays_interactive() {
        let invocation = build(CliOptions {
            harness: "muse".to_string(),
            permission_mode: Some("never".to_string()),
            ..CliOptions::default()
        })
        .unwrap();

        assert_eq!(invocation.args, vec!["--approval-mode", "never"]);
        assert!(!invocation.args.iter().any(|arg| arg == "exec"));
    }

    #[test]
    fn approval_mode_is_dropped_headlessly() {
        // `muse exec` errors with "unknown option --approval-mode".
        let invocation = build(CliOptions {
            harness: "muse".to_string(),
            permission_mode: Some("never".to_string()),
            prompt: Some("go".to_string()),
            ..CliOptions::default()
        })
        .unwrap();

        assert!(!invocation.args.iter().any(|arg| arg == "--approval-mode"));
    }

    #[test]
    fn headless_resume_reuses_the_session_id() {
        let resumed = build(CliOptions {
            harness: "muse".to_string(),
            prompt: Some("more".to_string()),
            // resume wins over session_id when both are present
            session_id: Some("sess-1".to_string()),
            resume_id: Some("sess-2".to_string()),
            ..CliOptions::default()
        })
        .unwrap();
        assert!(resumed
            .args
            .windows(2)
            .any(|w| w == ["--session-id".to_string(), "sess-2".to_string()]));

        let fresh = build(CliOptions {
            harness: "muse".to_string(),
            prompt: Some("start".to_string()),
            session_id: Some("sess-1".to_string()),
            ..CliOptions::default()
        })
        .unwrap();
        assert!(fresh
            .args
            .windows(2)
            .any(|w| w == ["--session-id".to_string(), "sess-1".to_string()]));
    }

    #[test]
    fn headless_resume_latest_is_rejected() {
        let error = build(CliOptions {
            harness: "muse".to_string(),
            prompt: Some("go".to_string()),
            resume_id: Some("latest".to_string()),
            ..CliOptions::default()
        })
        .unwrap_err();
        assert!(error.contains("resume the latest session"), "{error}");
    }

    #[test]
    fn interactive_resume_uses_the_resume_subcommand() {
        let last = build(CliOptions {
            harness: "muse".to_string(),
            resume_id: Some("last".to_string()),
            ..CliOptions::default()
        })
        .unwrap();
        assert_eq!(last.args, vec!["resume", "--last"]);

        let by_id = build(CliOptions {
            harness: "muse".to_string(),
            resume_id: Some("sess-3".to_string()),
            ..CliOptions::default()
        })
        .unwrap();
        assert_eq!(by_id.args, vec!["resume", "sess-3"]);
    }
}
