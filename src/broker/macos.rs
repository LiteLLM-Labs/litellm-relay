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
        env, fs,
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
    };
    use uuid::Uuid;

    const TEST_IDENTIFIER: &str = "relay.test.caller";

    /// `/bin/sh` on macOS is a launcher that execs the selected shell, which
    /// would drop the ad hoc signature; zsh is a plain binary.
    const SHELL: &str = "/bin/zsh";

    fn scratch_dir() -> PathBuf {
        let dir = env::temp_dir().join(format!("relay-broker-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn ad_hoc_signed_shell(dir: &Path) -> PathBuf {
        let shell = dir.join("signed-sh");
        fs::copy(SHELL, &shell).expect("copy the shell");
        let status = Command::new("codesign")
            .args(["-s", "-", "-f", "-i", TEST_IDENTIFIER])
            .arg(&shell)
            .status()
            .expect("codesign");
        assert!(status.success(), "ad hoc codesign failed");
        shell
    }

    struct Sleeper {
        shell: Child,
        sleep_pid: u32,
    }

    impl Sleeper {
        /// The shell prints the background job's pid before that child has
        /// exec'ed `sleep`, so the walk would otherwise see a forked copy of
        /// the shell at level 0; wait until the pid runs `sleep`.
        fn spawn(shell: &Path) -> Self {
            let mut child = Command::new(shell)
                .args(["-c", "sleep 30 & echo $!; wait"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn shell");
            let stdout = child.stdout.take().expect("stdout");
            let mut line = String::new();
            BufReader::new(stdout)
                .read_line(&mut line)
                .expect("sleep pid");
            let sleep_pid = line.trim().parse().expect("pid");
            for _ in 0..500 {
                if executable_of(sleep_pid).ends_with("/sleep") {
                    return Self {
                        shell: child,
                        sleep_pid,
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("the background job never exec'ed sleep");
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
            let _ = self.shell.kill();
            let _ = self.shell.wait();
        }
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
    fn should_allow_a_child_of_an_ad_hoc_signed_shell_by_identifier_and_refuse_it_by_team() {
        let dir = scratch_dir();
        let shell = ad_hoc_signed_shell(&dir);
        let sleeper = Sleeper::spawn(&shell);
        let table = MacProcessTable;
        let by_identifier = vec![AllowedCaller::new(TEST_IDENTIFIER, None)];
        assert!(matches!(
            walk(&table, &by_identifier, peer(sleeper.sleep_pid)),
            Verdict::Allowed { level: 1, .. }
        ));
        let by_team = vec![AllowedCaller::new(TEST_IDENTIFIER, Some("Q6L2SF6YDW"))];
        let refused = walk(&table, &by_team, peer(sleeper.sleep_pid));
        let message = refused.refusal_message().expect("refused by team");
        assert!(message.contains(TEST_IDENTIFIER), "{message}");
        drop(sleeper);
        let plain = Sleeper::spawn(&PathBuf::from(SHELL));
        assert!(matches!(
            walk(&table, &by_identifier, peer(plain.sleep_pid)),
            Verdict::Refused { .. }
        ));
        drop(plain);
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn should_validate_the_peer_through_its_audit_token() {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let token = peer_audit_token(std::os::fd::AsRawFd::as_raw_fd(&ours)).expect("audit token");
        let own_identifier = SecCode::for_self(Flags::NONE)
            .ok()
            .and_then(|code| signing_information(&code))
            .map(|(identifier, _)| identifier)
            .expect("test binary is ad hoc signed");
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
