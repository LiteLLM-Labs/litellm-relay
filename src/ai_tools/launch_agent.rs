use std::path::Path;

use anyhow::{anyhow, Result};

use crate::{ai_tools::credential::LAUNCH_AGENT_LABEL, broker::socket_path, config::relay_home};

const ANSWER_POLLS: u32 = 150;
const LOADED_GRACE_POLLS: u32 = 30;
const ANSWER_WAIT_SECONDS: u32 = 15;
const OUT_LOG: &str = "launchd.out.log";
const ERR_LOG: &str = "launchd.err.log";

pub trait DaemonHost {
    fn answers(&self) -> bool;
    fn agent_loaded(&self) -> bool;
    fn install_agent(&self) -> Result<(), String>;
    fn restart_agent(&self) -> Result<(), String>;
    fn pause(&self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Daemon {
    AlreadyAnswering,
    Started,
    Restarted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartFailure {
    Install(String),
    Restart(String),
    NoAnswer(Daemon),
}

pub fn ensure_daemon(host: &dyn DaemonHost) -> Result<Daemon, StartFailure> {
    if host.answers() {
        return Ok(Daemon::AlreadyAnswering);
    }
    let started = if host.agent_loaded() {
        if answers_within(host, LOADED_GRACE_POLLS) {
            return Ok(Daemon::AlreadyAnswering);
        }
        host.restart_agent().map_err(StartFailure::Restart)?;
        Daemon::Restarted
    } else {
        host.install_agent().map_err(StartFailure::Install)?;
        Daemon::Started
    };
    if answers_within(host, ANSWER_POLLS) {
        return Ok(started);
    }
    Err(StartFailure::NoAnswer(started))
}

fn answers_within(host: &dyn DaemonHost, polls: u32) -> bool {
    (0..polls).any(|_| {
        host.pause();
        host.answers()
    })
}

pub fn require_daemon(host: &dyn DaemonHost, quiet: bool) -> Result<()> {
    let daemon = ensure_daemon(host)
        .map_err(|failure| anyhow!(describe(&failure, &socket_path(), &relay_home())))?;
    match daemon {
        Daemon::AlreadyAnswering if quiet => {}
        Daemon::AlreadyAnswering => println!("The Relay daemon is already running."),
        Daemon::Started => println!(
            "Started the Relay daemon as the {LAUNCH_AGENT_LABEL} LaunchAgent; launchd starts it again at login."
        ),
        Daemon::Restarted => println!(
            "Restarted the {LAUNCH_AGENT_LABEL} LaunchAgent; the Relay daemon is answering."
        ),
    }
    Ok(())
}

pub fn describe(failure: &StartFailure, socket: &Path, relay_home: &Path) -> String {
    let log = relay_home.join(ERR_LOG);
    match failure {
        StartFailure::Install(reason) => format!(
            "could not start the Relay daemon: installing the {LAUNCH_AGENT_LABEL} LaunchAgent failed: {reason}"
        ),
        StartFailure::Restart(reason) => format!(
            "could not start the Relay daemon: the {LAUNCH_AGENT_LABEL} LaunchAgent is loaded and restarting it failed: {reason}"
        ),
        StartFailure::NoAnswer(Daemon::Restarted) => format!(
            "could not start the Relay daemon: the {LAUNCH_AGENT_LABEL} LaunchAgent was already loaded and was restarted, but nothing answered on {} within {ANSWER_WAIT_SECONDS} seconds; it may serve another Relay home, see {}",
            socket.display(),
            log.display()
        ),
        StartFailure::NoAnswer(_) => format!(
            "could not start the Relay daemon: the {LAUNCH_AGENT_LABEL} LaunchAgent was installed, but nothing answered on {} within {ANSWER_WAIT_SECONDS} seconds; see {}",
            socket.display(),
            log.display()
        ),
    }
}

pub fn render_plist(executable: &str, home: &str) -> String {
    let executable = xml_text(executable);
    let home = xml_text(home);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>serve</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key>
    <string>{home}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{home}/.litellm-relay/{OUT_LOG}</string>
  <key>StandardErrorPath</key>
  <string>{home}/.litellm-relay/{ERR_LOG}</string>
</dict>
</plist>
"#
    )
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub struct Launchd;

const LAUNCHD_SERVICE_VARIABLE: &str = "XPC_SERVICE_NAME";

pub fn runs_as_agent() -> bool {
    launched_as(std::env::var(LAUNCHD_SERVICE_VARIABLE).ok().as_deref())
}

fn launched_as(service: Option<&str>) -> bool {
    service == Some(LAUNCH_AGENT_LABEL)
}

pub fn host_plist() -> Result<String> {
    live::plist().map_err(|reason| anyhow!(reason))
}

#[cfg(unix)]
mod live {
    use std::{
        fs,
        os::unix::fs::{chown, MetadataExt},
        path::{Path, PathBuf},
        process::Command,
        thread,
        time::Duration,
    };

    use super::{render_plist, DaemonHost, Launchd, ANSWER_POLLS, ANSWER_WAIT_SECONDS};
    use crate::{
        ai_tools::credential::{daemon_answers, relay_executable, LAUNCH_AGENT_LABEL},
        broker::socket_path,
        system::home_dir,
    };

    const LAUNCHCTL: &str = "/bin/launchctl";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Owner {
        pub(super) uid: u32,
        pub(super) gid: u32,
    }

    fn absolute_home() -> Result<String, String> {
        let home = home_dir();
        match home.to_str() {
            Some(text) if home.is_absolute() => Ok(text.to_string()),
            _ => Err(format!(
                "HOME ({}) must be an absolute UTF-8 path",
                home.display()
            )),
        }
    }

    pub(super) fn plist() -> Result<String, String> {
        let executable = relay_executable().map_err(|error| format!("{error:#}"))?;
        Ok(render_plist(&executable, &absolute_home()?))
    }

    fn home_owner(home: &str) -> Result<Owner, String> {
        let metadata =
            fs::metadata(home).map_err(|error| format!("could not read {home}: {error}"))?;
        Ok(Owner {
            uid: metadata.uid(),
            gid: metadata.gid(),
        })
    }

    fn service(owner: Owner) -> String {
        format!("gui/{}/{LAUNCH_AGENT_LABEL}", owner.uid)
    }

    fn launchctl(args: &[&str]) -> Result<(), String> {
        let output = Command::new(LAUNCHCTL)
            .args(args)
            .output()
            .map_err(|error| format!("could not run {LAUNCHCTL}: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "`launchctl {}` failed: {}",
            args.join(" "),
            stderr.lines().next().unwrap_or("no error text").trim()
        ))
    }

    pub(super) fn write_plist(path: &Path, contents: &str, owner: Owner) -> Result<(), String> {
        let dir = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
        let failed = |what: &Path, error: std::io::Error| format!("{}: {error}", what.display());
        if !dir.exists() {
            fs::create_dir_all(dir).map_err(|error| failed(dir, error))?;
            chown(dir, Some(owner.uid), Some(owner.gid)).map_err(|error| failed(dir, error))?;
        }
        if fs::read_to_string(path).ok().as_deref() == Some(contents) {
            return Ok(());
        }
        fs::write(path, contents).map_err(|error| failed(path, error))?;
        chown(path, Some(owner.uid), Some(owner.gid)).map_err(|error| failed(path, error))
    }

    impl DaemonHost for Launchd {
        fn answers(&self) -> bool {
            daemon_answers(&socket_path())
        }

        fn agent_loaded(&self) -> bool {
            absolute_home()
                .and_then(|home| home_owner(&home))
                .and_then(|owner| launchctl(&["print", &service(owner)]))
                .is_ok()
        }

        fn install_agent(&self) -> Result<(), String> {
            let home = absolute_home()?;
            let owner = home_owner(&home)?;
            let path = PathBuf::from(&home)
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LAUNCH_AGENT_LABEL}.plist"));
            write_plist(&path, &plist()?, owner)?;
            launchctl(&["enable", &service(owner)])?;
            launchctl(&[
                "bootstrap",
                &format!("gui/{}", owner.uid),
                &path.display().to_string(),
            ])
        }

        fn restart_agent(&self) -> Result<(), String> {
            let owner = home_owner(&absolute_home()?)?;
            launchctl(&["kickstart", "-k", &service(owner)])
        }

        fn pause(&self) {
            thread::sleep(Duration::from_secs(u64::from(ANSWER_WAIT_SECONDS)) / ANSWER_POLLS);
        }
    }
}

#[cfg(not(unix))]
mod live {
    use super::{DaemonHost, Launchd};

    const UNSUPPORTED: &str = "LaunchAgents exist on macOS only";

    pub(super) fn plist() -> Result<String, String> {
        Err(UNSUPPORTED.to_string())
    }

    impl DaemonHost for Launchd {
        fn answers(&self) -> bool {
            false
        }

        fn agent_loaded(&self) -> bool {
            false
        }

        fn install_agent(&self) -> Result<(), String> {
            Err(UNSUPPORTED.to_string())
        }

        fn restart_agent(&self) -> Result<(), String> {
            Err(UNSUPPORTED.to_string())
        }

        fn pause(&self) {}
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{cell::RefCell, collections::VecDeque, sync::Mutex};

    use super::DaemonHost;

    pub(crate) static HOME_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) struct FakeHost {
        answers: RefCell<VecDeque<bool>>,
        answers_after_start: bool,
        loaded: bool,
        launchd: Result<(), String>,
        pub(crate) calls: RefCell<Vec<&'static str>>,
    }

    impl FakeHost {
        pub(crate) fn down() -> Self {
            Self {
                answers: RefCell::new(VecDeque::new()),
                answers_after_start: true,
                loaded: false,
                launchd: Ok(()),
                calls: RefCell::new(Vec::new()),
            }
        }

        pub(crate) fn answering() -> Self {
            Self::down().then_answers(&[true])
        }

        pub(crate) fn then_answers(self, script: &[bool]) -> Self {
            Self {
                answers: RefCell::new(script.iter().copied().collect()),
                ..self
            }
        }

        pub(crate) fn never_answering(self) -> Self {
            Self {
                answers_after_start: false,
                ..self
            }
        }

        pub(crate) fn loaded(self) -> Self {
            Self {
                loaded: true,
                ..self
            }
        }

        pub(crate) fn launchd_failing(self, reason: &str) -> Self {
            Self {
                launchd: Err(reason.to_string()),
                ..self
            }
        }

        pub(crate) fn count(&self, call: &str) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|seen| **seen == call)
                .count()
        }

        fn started(&self) -> bool {
            self.count("install_agent") + self.count("restart_agent") > 0
        }
    }

    impl DaemonHost for FakeHost {
        fn answers(&self) -> bool {
            self.calls.borrow_mut().push("answers");
            let scripted = self.answers.borrow_mut().pop_front();
            scripted.unwrap_or(self.started() && self.answers_after_start)
        }

        fn agent_loaded(&self) -> bool {
            self.calls.borrow_mut().push("agent_loaded");
            self.loaded
        }

        fn install_agent(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("install_agent");
            self.launchd.clone()
        }

        fn restart_agent(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("restart_agent");
            self.launchd.clone()
        }

        fn pause(&self) {
            self.calls.borrow_mut().push("pause");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{test_support::FakeHost, *};

    #[test]
    fn should_change_nothing_when_a_daemon_already_answers() {
        let host = FakeHost::answering().loaded();
        assert_eq!(ensure_daemon(&host), Ok(Daemon::AlreadyAnswering));
        assert_eq!(*host.calls.borrow(), vec!["answers"]);
    }

    #[test]
    fn should_install_the_agent_and_wait_until_the_daemon_answers() {
        let host = FakeHost::down().then_answers(&[false, false, false]);
        assert_eq!(ensure_daemon(&host), Ok(Daemon::Started));
        assert_eq!(host.count("install_agent"), 1);
        assert_eq!(host.count("restart_agent"), 0);
        assert_eq!(host.count("pause"), 3);
        assert_eq!(host.calls.borrow().last(), Some(&"answers"));
    }

    #[test]
    fn should_restart_a_loaded_agent_without_installing_over_it() {
        let host = FakeHost::down().loaded();
        assert_eq!(ensure_daemon(&host), Ok(Daemon::Restarted));
        assert_eq!(host.count("restart_agent"), 1);
        assert_eq!(host.count("install_agent"), 0);
        assert_eq!(host.count("pause"), LOADED_GRACE_POLLS as usize + 1);
    }

    #[test]
    fn should_leave_a_loaded_agent_alone_while_its_daemon_is_still_starting() {
        let host = FakeHost::down()
            .loaded()
            .then_answers(&[false, false, true]);
        assert_eq!(ensure_daemon(&host), Ok(Daemon::AlreadyAnswering));
        assert_eq!(host.count("restart_agent"), 0);
        assert_eq!(host.count("install_agent"), 0);
        assert_eq!(host.count("pause"), 2);
    }

    #[test]
    fn should_start_the_agent_once_across_repeated_runs() {
        let host = FakeHost::down();
        assert_eq!(ensure_daemon(&host), Ok(Daemon::Started));
        assert_eq!(ensure_daemon(&host), Ok(Daemon::AlreadyAnswering));
        assert_eq!(ensure_daemon(&host), Ok(Daemon::AlreadyAnswering));
        assert_eq!(host.count("install_agent"), 1);
        assert_eq!(host.count("agent_loaded"), 1);
    }

    #[test]
    fn should_fail_when_nothing_answers_after_the_start() {
        let installed = FakeHost::down().never_answering();
        assert_eq!(
            ensure_daemon(&installed),
            Err(StartFailure::NoAnswer(Daemon::Started))
        );
        assert_eq!(installed.count("pause"), ANSWER_POLLS as usize);

        let restarted = FakeHost::down().loaded().never_answering();
        assert_eq!(
            ensure_daemon(&restarted),
            Err(StartFailure::NoAnswer(Daemon::Restarted))
        );
    }

    #[test]
    fn should_report_a_launchd_failure_without_waiting_for_an_answer() {
        let install = FakeHost::down().launchd_failing("Bootstrap failed: 5");
        assert_eq!(
            ensure_daemon(&install),
            Err(StartFailure::Install("Bootstrap failed: 5".into()))
        );
        assert_eq!(install.count("pause"), 0);

        let restart = FakeHost::down().loaded().launchd_failing("no such process");
        assert_eq!(
            ensure_daemon(&restart),
            Err(StartFailure::Restart("no such process".into()))
        );
        assert_eq!(restart.count("pause"), LOADED_GRACE_POLLS as usize);
    }

    #[test]
    fn should_fail_the_command_with_one_line_naming_the_socket_and_the_log() {
        let socket = PathBuf::from("/Users/dev/.litellm-relay/broker.sock");
        let home = PathBuf::from("/Users/dev/.litellm-relay");
        for failure in [
            StartFailure::Install("Bootstrap failed: 5".into()),
            StartFailure::Restart("no such process".into()),
            StartFailure::NoAnswer(Daemon::Started),
            StartFailure::NoAnswer(Daemon::Restarted),
        ] {
            let line = describe(&failure, &socket, &home);
            assert!(line.starts_with("could not start the Relay daemon: "));
            assert!(line.contains(LAUNCH_AGENT_LABEL));
            assert_eq!(line.lines().count(), 1);
        }
        let silent = describe(&StartFailure::NoAnswer(Daemon::Started), &socket, &home);
        assert!(silent.contains("/Users/dev/.litellm-relay/broker.sock"));
        assert!(silent.contains("/Users/dev/.litellm-relay/launchd.err.log"));
        let foreign = describe(&StartFailure::NoAnswer(Daemon::Restarted), &socket, &home);
        assert!(foreign.contains("another Relay home"));

        let error = require_daemon(&FakeHost::down().launchd_failing("denied"), true)
            .expect_err("a failed start must fail the command");
        assert!(error.to_string().ends_with("failed: denied"));
        assert!(require_daemon(&FakeHost::down(), true).is_ok());
    }

    #[test]
    fn should_recognize_only_the_process_launchd_started_for_the_label() {
        assert!(launched_as(Some(LAUNCH_AGENT_LABEL)));
        assert!(!launched_as(Some("0")));
        assert!(!launched_as(Some("ai.litellm.relay.autoconfigure")));
        assert!(!launched_as(None));
    }

    #[test]
    fn should_render_an_agent_that_serves_the_home_the_command_ran_with() {
        let rendered = render_plist("/opt/relay & co/litellm-relay", "/Users/dev<1>");
        let parsed: plist::Value = plist::from_bytes(rendered.as_bytes()).unwrap();
        let agent = parsed.as_dictionary().unwrap();
        assert_eq!(agent["Label"].as_string(), Some(LAUNCH_AGENT_LABEL));
        let arguments: Vec<_> = agent["ProgramArguments"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(plist::Value::as_string)
            .collect();
        assert_eq!(arguments, vec!["/opt/relay & co/litellm-relay", "serve"]);
        assert_eq!(
            agent["EnvironmentVariables"].as_dictionary().unwrap()["HOME"].as_string(),
            Some("/Users/dev<1>")
        );
        assert_eq!(agent["RunAtLoad"].as_boolean(), Some(true));
        assert_eq!(agent["KeepAlive"].as_boolean(), Some(true));
        assert_eq!(
            agent["StandardErrorPath"].as_string(),
            Some("/Users/dev<1>/.litellm-relay/launchd.err.log")
        );
    }

    #[cfg(unix)]
    #[test]
    fn should_write_the_plist_once_and_replace_a_different_one() {
        use std::{fs, os::unix::fs::MetadataExt};

        let dir = std::env::temp_dir().join(format!("relay-agent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let metadata = fs::metadata(&dir).unwrap();
        let owner = live::Owner {
            uid: metadata.uid(),
            gid: metadata.gid(),
        };
        let path = dir.join("Library").join("LaunchAgents").join("a.plist");

        live::write_plist(&path, "first", owner).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        let written = fs::metadata(&path).unwrap();
        assert_eq!(written.uid(), owner.uid);

        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o444)).unwrap();
        live::write_plist(&path, "first", owner).unwrap();
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
        live::write_plist(&path, "second", owner).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        fs::remove_dir_all(&dir).unwrap();
    }
}
