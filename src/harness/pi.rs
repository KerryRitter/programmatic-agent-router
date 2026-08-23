use super::{add_passthrough, plain_model, resume_is_latest, Harness, Invocation, Request};

pub(crate) fn new() -> Box<dyn Harness> {
    Box::new(PiHarness)
}

struct PiHarness;

impl Harness for PiHarness {
    fn build(&self, request: &Request) -> Result<Invocation, String> {
        let mut args = Vec::new();

        if let Some(prompt) = &request.prompt {
            // Pi's `-p` is its non-interactive print mode. A positional prompt
            // opens the interactive TUI instead, which is unsuitable for ask
            // and fuse's captured runs.
            args.extend(["-p".to_string(), prompt.clone()]);
        }

        if let Some(provider) = &request.provider {
            args.extend(["--provider".to_string(), provider.clone()]);
        }
        if let Some(model) = plain_model(request) {
            // Pi accepts either a plain model alongside `--provider` or a
            // provider-qualified model such as `openai/gpt-5.4`.
            args.extend(["--model".to_string(), model]);
        }

        if request.prompt.is_some() {
            if let Some(format) = &request.output_format {
                let mode = match format.as_str() {
                    "text" => "text",
                    // Pi's JSON output is an event stream, so both shared JSON
                    // spellings map to its one `json` mode.
                    "json" | "stream-json" => "json",
                    other => {
                        return Err(format!(
                            "pi supports --output-format text, json, and stream-json (got {other})"
                        ));
                    }
                };
                args.extend(["--mode".to_string(), mode.to_string()]);
            }
        }

        // Pi can continue its newest cwd-scoped session with `--continue`, or
        // open a named session with `--session`. A caller-supplied session id
        // sets the exact id when a fresh session is being created. Resume wins.
        match (&request.resume_id, &request.session_id) {
            (Some(resume), _) if resume_is_latest(resume) => args.push("--continue".to_string()),
            (Some(resume), _) => args.extend(["--session".to_string(), resume.clone()]),
            (None, Some(session)) => args.extend(["--session-id".to_string(), session.clone()]),
            (None, None) => {}
        }

        // Pi has no permission-bypass switch. In print mode it executes its
        // configured tools non-interactively; interactive project trust remains
        // owned by Pi itself.
        Ok(Invocation::new("pi", add_passthrough(args, request)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CliOptions;

    fn build(options: CliOptions) -> Result<Invocation, String> {
        let request = Request::from_options(options, String::new()).unwrap();
        PiHarness.build(&request)
    }

    #[test]
    fn prompt_uses_print_mode_and_pi_native_flags() {
        let invocation = build(CliOptions {
            harness: "pi".to_string(),
            provider: Some("openai".to_string()),
            model: Some("gpt-5.4".to_string()),
            output_format: Some("stream-json".to_string()),
            prompt: Some("review this".to_string()),
            yolo: true,
            ..CliOptions::default()
        })
        .unwrap();

        assert_eq!(invocation.command, "pi");
        assert_eq!(
            invocation.args,
            vec![
                "-p",
                "review this",
                "--provider",
                "openai",
                "--model",
                "gpt-5.4",
                "--mode",
                "json",
            ]
        );
    }

    #[test]
    fn resumes_by_id_or_continues_latest() {
        let by_id = build(CliOptions {
            harness: "pi".to_string(),
            prompt: Some("continue".to_string()),
            session_id: Some("fresh-id".to_string()),
            resume_id: Some("prior-id".to_string()),
            ..CliOptions::default()
        })
        .unwrap();
        assert!(by_id
            .args
            .windows(2)
            .any(|w| w == ["--session".to_string(), "prior-id".to_string()]));
        assert!(!by_id.args.iter().any(|arg| arg == "--session-id"));

        let latest = build(CliOptions {
            harness: "pi".to_string(),
            resume_id: Some("latest".to_string()),
            ..CliOptions::default()
        })
        .unwrap();
        assert_eq!(latest.args, vec!["--continue"]);
    }

    #[test]
    fn rejects_unsupported_output_format() {
        let error = build(CliOptions {
            harness: "pi".to_string(),
            output_format: Some("rpc".to_string()),
            prompt: Some("go".to_string()),
            ..CliOptions::default()
        })
        .unwrap_err();
        assert!(error.contains("text, json, and stream-json"), "{error}");
    }
}
