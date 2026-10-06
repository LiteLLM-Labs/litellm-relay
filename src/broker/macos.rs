//! The macOS process table behind the caller check: parent pids from
//! `proc_pidinfo`, code-signing checks through the Security framework, and the
//! peer's audit token from the socket.

use std::{ffi::c_void, mem, os::fd::RawFd, str::FromStr};

use core_foundation::{
    base::{CFType, CFTypeRef, OSStatus, TCFType},
    data::CFData,
    dictionary::{CFDictionary, CFDictionaryRef},
    string::{CFString, CFStringRef},
};
use security_framework::os::macos::code_signing::{
    Flags, GuestAttributes, SecCode, SecRequirement,
};

use super::caller::{ProcessTable, Subject, AUDIT_TOKEN_BYTES};

const SIGNING_INFORMATION_FLAG: u32 = 1 << 1;

#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecCodeInfoIdentifier: CFStringRef;
    static kSecCodeInfoTeamIdentifier: CFStringRef;
    fn SecCodeCopySigningInformation(
        code: CFTypeRef,
        flags: u32,
        information: *mut CFDictionaryRef,
    ) -> OSStatus;
}

pub struct MacProcessTable;

impl ProcessTable for MacProcessTable {
    fn parent(&self, pid: u32) -> Option<u32> {
        let Ok(pid) = i32::try_from(pid) else {
            return None;
        };
        let mut info = mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = mem::size_of::<libc::proc_bsdinfo>() as i32;
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr() as *mut c_void,
                size,
            )
        };
        if written != size {
            return None;
        }
        Some(unsafe { info.assume_init() }.pbi_ppid)
    }

    fn validates(&self, subject: Subject, requirement: &str) -> bool {
        let Ok(requirement) = SecRequirement::from_str(requirement) else {
            return false;
        };
        guest(subject).is_some_and(|code| code.check_validity(Flags::NONE, &requirement).is_ok())
    }

    fn describe(&self, subject: Subject) -> String {
        let Some(code) = guest(subject) else {
            return format!("{} (no such process)", label(subject));
        };
        match signing_information(&code) {
            Some((identifier, Some(team))) => format!("{identifier} ({team})"),
            Some((identifier, None)) => format!("{identifier} (no team)"),
            None => format!("{} (unsigned)", label(subject)),
        }
    }
}

fn label(subject: Subject) -> String {
    match subject {
        Subject::Pid(pid) => format!("pid {pid}"),
        Subject::AuditToken(_) => "peer".to_string(),
    }
}

fn guest(subject: Subject) -> Option<SecCode> {
    let mut attributes = GuestAttributes::new();
    let token = match subject {
        Subject::Pid(pid) => {
            attributes.set_pid(i32::try_from(pid).ok()?);
            None
        }
        Subject::AuditToken(token) => Some(CFData::from_buffer(&token)),
    };
    if let Some(token) = &token {
        attributes.set_audit_token(token.as_concrete_TypeRef());
    }
    SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE).ok()
}

fn signing_information(code: &SecCode) -> Option<(String, Option<String>)> {
    let mut raw: CFDictionaryRef = std::ptr::null();
    let status = unsafe {
        SecCodeCopySigningInformation(code.as_CFTypeRef(), SIGNING_INFORMATION_FLAG, &mut raw)
    };
    if status != 0 || raw.is_null() {
        return None;
    }
    let information: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(raw) };
    let identifier = string_value(&information, unsafe { kSecCodeInfoIdentifier })?;
    let team = string_value(&information, unsafe { kSecCodeInfoTeamIdentifier });
    Some((identifier, team))
}

fn string_value(information: &CFDictionary<CFString, CFType>, key: CFStringRef) -> Option<String> {
    let key = unsafe { CFString::wrap_under_get_rule(key) };
    information
        .find(&key)
        .and_then(|value| value.downcast::<CFString>())
        .map(|value| value.to_string())
}

/// The audit token of the process on the other end of a local socket, which
/// unlike a pid cannot be reused by another process after the peer exits.
pub fn peer_audit_token(fd: RawFd) -> Option<[u8; AUDIT_TOKEN_BYTES]> {
    let mut token = [0u8; AUDIT_TOKEN_BYTES];
    let mut length = AUDIT_TOKEN_BYTES as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr() as *mut c_void,
            &mut length,
        )
    };
    (status == 0 && length as usize == AUDIT_TOKEN_BYTES).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::caller::{walk, AllowedCaller, Peer, Verdict};
    use std::{
        env,
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
        process::{Child, Command, Stdio},
    };

    const SLEEPER_TEST: &str = "broker::macos::tests::sleeper_process";
    const SLEEP_PID_PREFIX: &str = "sleep_pid=";

    /// Run only as the child of `should_allow_an_ancestor_by_identifier_and_refuse_it_by_team`:
    /// starts `/bin/sleep` and reports its pid so the parent can walk from the
    /// sleeper up to this test binary, which cargo ad hoc signs with its own
    /// identifier and no team.
    #[test]
    #[ignore]
    fn sleeper_process() {
        let mut sleep = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        println!("{SLEEP_PID_PREFIX}{}", sleep.id());
        let _ = sleep.wait();
    }

    struct Sleeper {
        harness: Child,
        sleep_pid: u32,
    }

    impl Sleeper {
        /// Spawns this test binary running `sleeper_process`, reads the pid
        /// of its `/bin/sleep` child, and waits until that pid runs `sleep`.
        fn spawn() -> Self {
            let mut harness = Command::new(env::current_exe().expect("test binary"))
                .args(["--ignored", "--exact", SLEEPER_TEST, "--nocapture"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn the sleeper harness");
            let stdout = harness.stdout.take().expect("stdout");
            let sleep_pid = BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
                .find_map(|line| line.strip_prefix(SLEEP_PID_PREFIX)?.trim().parse().ok())
                .expect("the sleeper harness reported its sleep pid");
            for _ in 0..500 {
                if executable_of(sleep_pid).ends_with("/sleep") {
                    return Self { harness, sleep_pid };
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let _ = harness.kill();
            let _ = harness.wait();
            panic!("the sleeper harness never exec'ed sleep");
        }
    }

    fn executable_of(pid: u32) -> String {
        let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let length = unsafe {
            libc::proc_pidpath(
                pid as i32,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len() as u32,
            )
        };
        if length <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buffer[..length as usize]).into_owned()
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.sleep_pid as i32, libc::SIGKILL);
            }
            let _ = self.harness.kill();
            let _ = self.harness.wait();
        }
    }

    fn own_identifier() -> String {
        SecCode::for_self(Flags::NONE)
            .ok()
            .and_then(|code| signing_information(&code))
            .map(|(identifier, _)| identifier)
            .expect("test binary is ad hoc signed")
    }

    fn peer(pid: u32) -> Peer {
        Peer {
            pid,
            audit_token: None,
        }
    }

    #[test]
    fn should_read_the_parent_pid_of_a_live_process() {
        let table = MacProcessTable;
        assert_eq!(
            table.parent(std::process::id()),
            Some(unsafe { libc::getppid() } as u32)
        );
        assert_eq!(table.parent(999_999_999), None);
    }

    #[test]
    fn should_allow_an_ancestor_by_identifier_and_refuse_it_by_team() {
        let sleeper = Sleeper::spawn();
        let table = MacProcessTable;
        let identifier = own_identifier();
        let by_identifier = vec![AllowedCaller::new(&identifier, None)];
        let verdict = walk(&table, &by_identifier, peer(sleeper.sleep_pid));
        assert!(
            matches!(verdict, Verdict::Allowed { level: 1, .. }),
            "{verdict:?}"
        );
        let by_team = vec![AllowedCaller::new(&identifier, Some("Q6L2SF6YDW"))];
        let refused = walk(&table, &by_team, peer(sleeper.sleep_pid));
        let message = refused.refusal_message().expect("refused by team");
        assert!(message.contains(&identifier), "{message}");
        let someone_else = vec![AllowedCaller::new("relay.test.someone-else", None)];
        assert!(matches!(
            walk(&table, &someone_else, peer(sleeper.sleep_pid)),
            Verdict::Refused { .. }
        ));
        drop(sleeper);
    }

    #[test]
    fn should_validate_the_peer_through_its_audit_token() {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let token = peer_audit_token(std::os::fd::AsRawFd::as_raw_fd(&ours)).expect("audit token");
        let own_identifier = own_identifier();
        let table = MacProcessTable;
        let allowed = vec![AllowedCaller::new(&own_identifier, None)];
        let verdict = walk(
            &table,
            &allowed,
            Peer {
                pid: std::process::id(),
                audit_token: Some(token),
            },
        );
        assert!(
            matches!(verdict, Verdict::Allowed { level: 0, .. }),
            "{verdict:?}"
        );
        let other = vec![AllowedCaller::new("relay.test.someone-else", None)];
        assert!(matches!(
            walk(&table, &other, peer(std::process::id())),
            Verdict::Refused { .. }
        ));
        drop(theirs);
    }
}
