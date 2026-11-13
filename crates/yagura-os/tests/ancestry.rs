#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts copies of this test binary as the processes whose lineage is read.
#![allow(clippy::disallowed_methods)]

//! A request is taken as a window's own only when the processes above it, up
//! to the window's PTY child, hold exactly one whose image the profile
//! declares: two are a nested CLI, and a lineage that cannot be traced to the
//! child is refused. A request with no anchor is not checked at all.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use yagura_os::{ProcessStart, process_start};
use yagura_test_support::paths::{TempDir, unique_suffix};
use yagura_test_support::process::{install_exe, libtest_name};

fn start(text: &str) -> ProcessStart {
    text.parse().expect("a start reads back from its string")
}

fn ancestor(pid: u32, image: &str, start_text: &str) -> yagura_os::ancestry::Ancestor {
    ancestor_named(pid, image.as_bytes(), start_text)
}

/// An ancestor whose name is the bytes `image`, as a Unix source gives it.
fn ancestor_named(pid: u32, image: &[u8], start_text: &str) -> yagura_os::ancestry::Ancestor {
    yagura_os::ancestry::Ancestor {
        pid,
        image: image.into(),
        start: start(start_text),
    }
}

fn verdict_name(verdict: &yagura_os::ancestry::LineageVerdict) -> &'static str {
    use yagura_os::ancestry::LineageVerdict;
    match verdict {
        LineageVerdict::Accepted => "accepted",
        LineageVerdict::Nested => "nested",
        LineageVerdict::Undecidable(_) => "undecidable",
        LineageVerdict::Unchecked => "unchecked",
    }
}

fn patterns(texts: &[&str]) -> Vec<yagura_os::ancestry::ImagePattern> {
    texts
        .iter()
        .map(|text| {
            text.parse()
                .unwrap_or_else(|_| panic!("the image pattern {text:?} is refused"))
        })
        .collect()
}

/// The verdict on `ancestors` (nearest first, the requester left out) with
/// the anchor `(pid, start)`.
fn judge(
    ancestors: Vec<yagura_os::ancestry::Ancestor>,
    rule: yagura_os::ancestry::NameRule,
    anchor: Option<(u32, &str)>,
    images: &[&str],
) -> &'static str {
    use yagura_os::ancestry::{Lineage, judge_lineage};
    let lineage = Lineage::new(ancestors, rule);
    let anchor = anchor.map(|(pid, text)| (pid, start(text)));
    let verdict = judge_lineage(
        &lineage,
        anchor.as_ref().map(|(pid, start)| (*pid, start)),
        &patterns(images),
    );
    verdict_name(&verdict)
}

/// Whether `image` counts under `pattern`: the image is the one ancestor,
/// and the anchor, so one match accepts and none leaves nothing to count.
fn counts(image: &str, pattern: &str, rule: yagura_os::ancestry::NameRule) -> bool {
    counts_named(image.as_bytes(), pattern, rule)
}

fn counts_named(image: &[u8], pattern: &str, rule: yagura_os::ancestry::NameRule) -> bool {
    match judge(
        vec![ancestor_named(7, image, "s7")],
        rule,
        Some((7, "s7")),
        &[pattern],
    ) {
        "accepted" => true,
        "undecidable" => false,
        other => panic!("one ancestor judged {other} under {pattern:?}"),
    }
}

#[test]
fn one_declared_image_up_to_the_pty_child_is_accepted() {
    use yagura_os::ancestry::NameRule;
    // hook shell -> the window's CLI -> the PTY child (a shim).
    let verdict = judge(
        vec![
            ancestor(11, "cmd.exe", "111"),
            ancestor(12, "agy.exe", "112"),
            ancestor(13, "cmd.exe", "113"),
        ],
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(verdict, "accepted");
}

#[test]
fn the_pty_child_itself_counts_as_an_ancestor() {
    use yagura_os::ancestry::NameRule;
    let verdict = judge(
        vec![
            ancestor(11, "cmd.exe", "111"),
            ancestor(12, "agy.exe", "112"),
        ],
        NameRule::Windows,
        Some((12, "112")),
        &["agy.exe"],
    );
    assert_eq!(verdict, "accepted");
}

#[test]
fn two_declared_images_are_a_nested_cli() {
    use yagura_os::ancestry::NameRule;
    // hook shell -> nested CLI -> tool shell -> the window's CLI -> PTY child.
    let verdict = judge(
        vec![
            ancestor(21, "cmd.exe", "121"),
            ancestor(22, "agy.exe", "122"),
            ancestor(23, "cmd.exe", "123"),
            ancestor(12, "agy.exe", "112"),
            ancestor(13, "cmd.exe", "113"),
        ],
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(verdict, "nested");
}

#[test]
fn images_above_the_pty_child_are_not_counted() {
    use yagura_os::ancestry::NameRule;
    let verdict = judge(
        vec![
            ancestor(11, "cmd.exe", "111"),
            ancestor(12, "agy.exe", "112"),
            ancestor(13, "cmd.exe", "113"),
            ancestor(90, "agy.exe", "190"),
        ],
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(verdict, "accepted");
}

#[test]
fn an_ancestor_matching_two_patterns_counts_once() {
    use yagura_os::ancestry::NameRule;
    let verdict = judge(
        vec![
            ancestor(11, "cmd.exe", "111"),
            ancestor(12, "agy.exe", "112"),
        ],
        NameRule::Windows,
        Some((12, "112")),
        &["agy.exe", "agy*"],
    );
    assert_eq!(verdict, "accepted");
}

#[test]
fn no_declared_image_up_to_the_pty_child_is_undecidable() {
    use yagura_os::ancestry::NameRule;
    let verdict = judge(
        vec![
            ancestor(11, "cmd.exe", "111"),
            ancestor(13, "cmd.exe", "113"),
        ],
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(verdict, "undecidable");
}

#[test]
fn a_lineage_that_never_reaches_the_pty_child_is_undecidable() {
    use yagura_os::ancestry::NameRule;
    // Out past the window: the holder, the daemon, the top of the tree.
    let verdict = judge(
        vec![
            ancestor(11, "cmd.exe", "111"),
            ancestor(12, "agy.exe", "112"),
            ancestor(40, "yagura-holder.exe", "140"),
            ancestor(41, "yagura-daemon.exe", "141"),
        ],
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(verdict, "undecidable");
    let verdict = judge(
        Vec::new(),
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(
        verdict, "undecidable",
        "a lineage the OS gave no step of was judged"
    );
}

#[test]
fn the_pty_childs_id_under_another_start_is_not_the_pty_child() {
    use yagura_os::ancestry::NameRule;
    let verdict = judge(
        vec![
            ancestor(11, "cmd.exe", "111"),
            ancestor(12, "agy.exe", "112"),
            ancestor(13, "cmd.exe", "999"),
        ],
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(verdict, "undecidable");
}

#[test]
fn a_pty_child_more_than_32_steps_up_is_undecidable() {
    use yagura_os::ancestry::NameRule;
    // The parent is the first step; the PTY child at step `steps` carries the
    // one declared image.
    let lineage_of = |steps: u32| {
        let mut ancestors: Vec<_> = (1..steps)
            .map(|step| ancestor(1000 + step, "cmd.exe", &format!("{step}")))
            .collect();
        ancestors.push(ancestor(13, "agy.exe", "113"));
        ancestors
    };
    let at_limit = judge(
        lineage_of(32),
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(at_limit, "accepted", "the PTY child at step 32 was refused");
    let past_limit = judge(
        lineage_of(33),
        NameRule::Windows,
        Some((13, "113")),
        &["agy.exe"],
    );
    assert_eq!(
        past_limit, "undecidable",
        "the PTY child at step 33 was judged"
    );
}

#[test]
fn a_request_without_an_anchor_is_not_checked() {
    use yagura_os::ancestry::NameRule;
    let nested = vec![
        ancestor(22, "agy.exe", "122"),
        ancestor(12, "agy.exe", "112"),
    ];
    assert_eq!(
        judge(nested, NameRule::Windows, None, &["agy.exe"]),
        "unchecked"
    );
    assert_eq!(
        judge(Vec::new(), NameRule::Unix, None, &["agy"]),
        "unchecked"
    );
}

/// WSL: the anchor is the root variable (`pid`, `<boot_id>:<starttime>`) the
/// window's command exports for the guest CLI.
#[test]
fn a_guest_lineage_is_counted_up_to_the_root_it_names() {
    use yagura_os::ancestry::NameRule;
    let root = Some((500, "boot-a:900"));
    let window_hook = judge(
        vec![
            ancestor(601, "sh", "boot-a:960"),
            ancestor(500, "agy", "boot-a:900"),
        ],
        NameRule::Unix,
        root,
        &["agy"],
    );
    assert_eq!(window_hook, "accepted");

    let nested_hook = judge(
        vec![
            ancestor(701, "sh", "boot-a:990"),
            ancestor(700, "agy", "boot-a:980"),
            ancestor(650, "bash", "boot-a:970"),
            ancestor(500, "agy", "boot-a:900"),
        ],
        NameRule::Unix,
        root,
        &["agy"],
    );
    assert_eq!(nested_hook, "nested");

    let root_restarted = judge(
        vec![
            ancestor(601, "sh", "boot-a:960"),
            ancestor(500, "agy", "boot-a:901"),
        ],
        NameRule::Unix,
        root,
        &["agy"],
    );
    assert_eq!(
        root_restarted, "undecidable",
        "the root's id under another start was taken for the root"
    );

    // The tool shell ended and the nested CLI was handed to PID 1.
    let reparented = judge(
        vec![
            ancestor(701, "sh", "boot-a:990"),
            ancestor(700, "agy", "boot-a:980"),
            ancestor(1, "init", "boot-a:1"),
        ],
        NameRule::Unix,
        root,
        &["agy"],
    );
    assert_eq!(reparented, "undecidable");

    let rootless = judge(
        vec![
            ancestor(601, "sh", "boot-a:960"),
            ancestor(500, "agy", "boot-a:900"),
        ],
        NameRule::Unix,
        None,
        &["agy"],
    );
    assert_eq!(rootless, "unchecked");
}

/// A `/proc/<pid>/stat` line of a guest process named `name`, whose parent is
/// `ppid` and whose start is `ticks`.
fn guest_stat(pid: u32, name: &str, ppid: u32, ticks: u32) -> String {
    format!("{pid} ({name}) S {ppid} {pid} {pid} 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {ticks} 0 0")
}

/// WSL: what a guest collected is one stat line per step, and the name each
/// step counts under is the one in its line, between the first `(` and the
/// last `)`, compared by the Unix rule.
#[test]
fn a_guest_step_counts_under_the_name_in_its_stat_line() {
    use yagura_os::ancestry::{GuestRoot, guest_lineage, judge_lineage};
    let root: GuestRoot = "500:boot-a:900".parse().unwrap();
    let verdict_of = |steps: &[String]| {
        let lineage = guest_lineage(&root, steps.iter().map(String::as_str));
        verdict_name(&judge_lineage(
            &lineage,
            Some(root.anchor()),
            &patterns(&["a) b (c"]),
        ))
    };
    let hook_shell = guest_stat(601, "sh", 500, 960);
    let cli = guest_stat(500, "a) b (c", 1, 900);
    let lineage = guest_lineage(&root, [hook_shell.as_str(), cli.as_str()]);
    assert_eq!(
        lineage
            .ancestors()
            .iter()
            .map(|step| &step.image[..])
            .collect::<Vec<_>>(),
        [&b"sh"[..], &b"a) b (c"[..]],
        "each step's name is the one in its stat line"
    );
    assert_eq!(verdict_of(&[hook_shell.clone(), cli.clone()]), "accepted");

    let nested = [
        guest_stat(701, "sh", 700, 990),
        guest_stat(700, "a) b (c", 650, 980),
        guest_stat(650, "bash", 500, 970),
        cli.clone(),
    ];
    assert_eq!(verdict_of(&nested), "nested");

    // A guest's kernel keeps 15 bytes of a name too.
    let cut = guest_stat(500, "Lineage-Probe-a", 1, 900);
    let lineage = guest_lineage(&root, [hook_shell.as_str(), cut.as_str()]);
    let verdict = judge_lineage(
        &lineage,
        Some(root.anchor()),
        &patterns(&["Lineage-Probe-abcdef"]),
    );
    assert_eq!(verdict_name(&verdict), "accepted");
}

#[test]
fn a_star_stands_for_any_run_of_characters_in_the_name() {
    use yagura_os::ancestry::NameRule;
    let case = NameRule::Unix;
    assert!(counts(
        "muse-bin-1.4.15-R8339.1.exe",
        "muse-bin-*.exe",
        case
    ));
    assert!(counts("muse-bin-.exe", "muse-bin-*.exe", case));
    assert!(counts("agy", "agy*", case));
    assert!(counts("agy", "a*y", case));

    assert!(!counts("cmd.exe", "muse-bin-*.exe", case));
    assert!(!counts("powershell.exe", "muse-bin-*.exe", case));
    assert!(!counts("muse-bin-1.4.15.exe.old", "muse-bin-*.exe", case));
    assert!(!counts("magy", "agy", case));
    assert!(!counts("agy2", "agy", case));
    // A name is compared whole: a process may name itself `x/agy`.
    assert!(!counts("x/agy", "agy", case));
}

#[test]
fn a_pattern_with_two_stars_or_an_empty_head_is_refused() {
    use yagura_os::ancestry::ImagePattern;
    assert!("muse-*-bin-*.exe".parse::<ImagePattern>().is_err());
    assert!("**".parse::<ImagePattern>().is_err());
    assert!("agy*".parse::<ImagePattern>().is_ok());
    // Every 15-byte Unix name would start with an empty head, so a window
    // holding one in its tree would read as nested.
    assert!("*agy".parse::<ImagePattern>().is_err());
    assert!("*".parse::<ImagePattern>().is_err());
}

/// Windows names fold case and Unix names do not. A Unix source keeps the
/// first 15 bytes of a name: a name of that length is compared with the
/// first 15 bytes of the pattern; a shorter one, and any name under the
/// Windows rule, is compared whole.
#[test]
fn windows_names_fold_case_and_unix_names_are_cut_to_15_bytes() {
    use yagura_os::ancestry::NameRule;
    assert!(counts("AGY.EXE", "agy.exe", NameRule::Windows));
    assert!(counts(
        "Muse-Bin-1.4.15.EXE",
        "muse-bin-*.exe",
        NameRule::Windows
    ));
    assert!(!counts("AGY", "agy", NameRule::Unix));
    assert!(!counts("Agy", "agy*", NameRule::Unix));

    let unix = NameRule::Unix;
    // `Lineage-Probe-a` is 15 bytes; `Lineage-Probe-` is 14.
    assert!(counts("Lineage-Probe-a", "Lineage-Probe-abcdef", unix));
    assert!(!counts("Lineage-Probe-b", "Lineage-Probe-abcdef", unix));
    assert!(
        !counts("Lineage-Probe-", "Lineage-Probe-abcdef", unix),
        "a 14-byte name was taken for a cut one"
    );
    assert!(
        !counts("agyagyagyagyagy", "agy", unix),
        "a pattern shorter than 15 bytes matched a 15-byte name"
    );
    assert!(counts("Lineage-Probe-a", "Lineage-Probe-a", unix));

    // A head of 15 bytes or more decides alone.
    assert!(counts("Lineage-Probe-a", "Lineage-Probe-ab*.bin", unix));
    assert!(!counts("Lineage-Probe-x", "Lineage-Probe-ab*.bin", unix));
    // A shorter head: the name starts with it, and what follows the star was
    // cut off, so it is not looked at.
    assert!(counts("muse-bin-1.4.15", "muse-bin-*.exe", unix));
    assert!(!counts("muse-bix-1.4.15", "muse-bin-*.exe", unix));
    assert!(counts("abXXXXXXXXXXXXX", "ab*cd", unix));
    assert!(!counts("abXXXXXXXXXXXX", "ab*cd", unix));

    // Windows names are never cut.
    let windows = NameRule::Windows;
    assert!(!counts("Lineage-Probe-a", "Lineage-Probe-abcdef", windows));
    assert!(!counts("Lineage-Probe-a", "Lineage-Probe-ab*", windows));
    assert!(counts(
        "LINEAGE-PROBE-ABCDEF.EXE",
        "lineage-probe-abcdef.exe",
        windows
    ));

    // A pattern whose 15th byte falls inside a character.
    let cut = "Lineage-Probe-\u{e9}";
    assert!(counts_named(&cut.as_bytes()[..15], cut, unix));
    assert!(!counts("Lineage-Probe-e", cut, unix));
    assert!(!counts_named(&cut.as_bytes()[..15], cut, windows));
    assert!(!counts("Lineage-Probe-e", "Lineage-Probe-\u{e9}*", unix));
}

/// Windows: the parent id of a snapshot outlives the parent, so a "parent"
/// created after the child is another process that took the id.
#[test]
fn a_windows_parent_created_after_its_child_is_not_its_parent() {
    use yagura_os::ancestry::windows_link_holds;
    assert!(windows_link_holds(100, 200));
    assert!(
        windows_link_holds(200, 200),
        "an equal creation time was refused"
    );
    assert!(!windows_link_holds(201, 200));
    // Raw 64-bit values, not their decimal strings: "10" sorts before "9".
    assert!(!windows_link_holds(10, 9));
    assert!(windows_link_holds(9, 10));
    let high = 1_u64 << 60;
    assert!(windows_link_holds(high, high + 1));
    assert!(!windows_link_holds(high + 1, high));
}

/// Unix: the parent read is the child's real parent only when the child's
/// ppid and start read before it and after it are both unchanged.
#[test]
fn a_unix_parent_read_holds_only_when_the_child_reads_the_same_on_both_sides() {
    use yagura_os::ancestry::unix_link_holds;
    let child = start("boot-a:900");
    assert!(unix_link_holds((40, &child), (40, &child)));
    assert!(
        !unix_link_holds((40, &child), (1, &child)),
        "a child handed to PID 1 between the reads kept its parent"
    );
    let reused = start("boot-a:950");
    assert!(
        !unix_link_holds((40, &child), (40, &reused)),
        "a child id taken over between the reads kept its parent"
    );
    // Starts are compared for equality only, never by magnitude.
    let earlier = start("boot-a:10");
    let later = start("boot-a:9");
    assert!(!unix_link_holds((40, &earlier), (40, &later)));
    assert!(!unix_link_holds((40, &later), (40, &earlier)));
}

// The real lineage: a chain of copies of this test binary under a name only
// these tests use, the leaf announcing its id on stdout once it runs.

const LINK: &str = "lineage_link";
const LINK_ROLE_ENV: &str = "YAGURA_OS_TEST_LINEAGE_ROLE";
const LEAF_MARK: &str = "yagura-lineage-leaf ";

/// One process of a chain: `relay:<n>` starts `relay:<n - 1>` (`leaf` after
/// `relay:0`) and waits for it; `leaf` announces its id and lives until its
/// stdin ends; `orphan` starts a leaf and exits at once.
#[test]
#[ignore = "a process of the lineage tests"]
fn lineage_link() {
    let Some(role) = std::env::var_os(LINK_ROLE_ENV) else {
        return;
    };
    let role = role.into_string().unwrap();
    let exe = std::env::current_exe().unwrap();
    match role.as_str() {
        "leaf" => {
            let mut out = std::io::stdout().lock();
            writeln!(out, "{LEAF_MARK}{}", std::process::id()).unwrap();
            out.flush().unwrap();
            drop(out);
            std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink()).unwrap();
        }
        // Left unwaited on purpose: this process exits at once so the OS hands
        // the leaf on.
        #[allow(clippy::zombie_processes)]
        "orphan" => {
            spawn_link(&exe, "leaf", Stdio::inherit(), Stdio::inherit());
        }
        relay => {
            let depth: u32 = relay
                .strip_prefix("relay:")
                .and_then(|depth| depth.parse().ok())
                .unwrap_or_else(|| panic!("an unknown lineage role {relay:?}"));
            let next = match depth {
                0 => "leaf".to_owned(),
                depth => format!("relay:{}", depth - 1),
            };
            let status = spawn_link(&exe, &next, Stdio::inherit(), Stdio::inherit())
                .wait()
                .unwrap();
            assert!(status.success(), "the next link failed: {status}");
        }
    }
}

fn spawn_link(exe: &Path, role: &str, stdin: Stdio, stdout: Stdio) -> Child {
    Command::new(exe)
        .args([
            libtest_name(module_path!(), LINK).as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LINK_ROLE_ENV, role)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .expect("start a lineage link")
}

/// A running chain started from this test process. Ending it closes the
/// leaf's stdin, so every link exits.
struct Chain {
    first: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    leaf: u32,
    image: String,
    exe: PathBuf,
    _dir: TempDir,
}

impl Chain {
    fn start(label: &str, role: &str) -> Self {
        let dir = TempDir::new("yagura-os-ancestry", label);
        let image = format!(
            "Lineage-Probe-{}{}",
            unique_suffix(),
            std::env::consts::EXE_SUFFIX
        );
        let exe: PathBuf = dir.path().join(&image);
        install_exe(&std::env::current_exe().unwrap(), &exe);
        let mut first = spawn_link(&exe, role, Stdio::piped(), Stdio::piped());
        let stdin = first.stdin.take();
        let mut stdout = BufReader::new(first.stdout.take().unwrap());
        let mut line = String::new();
        let leaf = loop {
            line.clear();
            let read = stdout.read_line(&mut line).unwrap();
            assert_ne!(read, 0, "the chain ended before its leaf announced itself");
            // libtest's own progress shares the line the mark is written on.
            if let Some((_, pid)) = line.trim_end().split_once(LEAF_MARK) {
                break pid.parse().unwrap();
            }
        };
        Self {
            first,
            stdin,
            stdout,
            leaf,
            image,
            exe,
            _dir: dir,
        }
    }

    /// Does to the chain's executable, while its links run, what a CLI does
    /// to its own file to update itself.
    fn update_file(&self, how: Update) {
        let new_file = std::env::current_exe().unwrap();
        match how {
            Update::RenamedAside => {
                let aside = self.exe.with_file_name(format!("{}.old", self.image));
                std::fs::rename(&self.exe, aside).unwrap();
                install_exe(&new_file, &self.exe);
            }
            Update::ReplacedOver => {
                let staged = self.exe.with_file_name(format!("{}.new", self.image));
                install_exe(&new_file, &staged);
                std::fs::rename(staged, &self.exe).unwrap();
            }
            Update::Unlinked => std::fs::remove_file(&self.exe).unwrap(),
        }
    }
}

/// How a CLI replaces its own executable to update itself.
#[derive(Debug, Clone, Copy)]
enum Update {
    /// Renames the running file aside and puts the new one under its name.
    RenamedAside,
    /// Renames the new file over the running one (refused by Windows).
    ReplacedOver,
    /// Removes the running file (refused by Windows).
    Unlinked,
}

impl Drop for Chain {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let mut rest = Vec::new();
        let _ = self.stdout.read_to_end(&mut rest);
        let _ = self.first.wait();
    }
}

/// The name the OS keeps of a process started from the file `image`: all of
/// it on Windows, its first 15 bytes on Unix.
fn kept_name(image: &str) -> &[u8] {
    if cfg!(windows) {
        image.as_bytes()
    } else {
        &image.as_bytes()[..image.len().min(15)]
    }
}

fn same_name(read: &[u8], expected: &[u8]) -> bool {
    if cfg!(windows) {
        read.eq_ignore_ascii_case(expected)
    } else {
        read == expected
    }
}

fn own_anchor() -> (u32, ProcessStart) {
    let pid = std::process::id();
    let start = process_start(pid)
        .unwrap()
        .expect("this test process has a start");
    (pid, start)
}

#[test]
fn the_os_reads_each_parent_its_image_and_its_start() {
    use yagura_os::ancestry::{judge_lineage, read_lineage};
    let chain = Chain::start("one", "relay:0");
    let lineage = read_lineage(chain.leaf);
    let ancestors = lineage.ancestors();
    assert!(
        ancestors.len() >= 2,
        "the lineage stopped short of this test process: {} steps",
        ancestors.len()
    );

    let relay = &ancestors[0];
    assert_eq!(relay.pid, chain.first.id(), "the leaf's parent was misread");
    assert_eq!(
        Some(relay.start.clone()),
        process_start(relay.pid).unwrap(),
        "the parent's start was misread"
    );
    assert!(
        same_name(&relay.image[..], kept_name(&chain.image)),
        "the parent's image read {:?}, not the name the OS keeps of {:?}",
        String::from_utf8_lossy(&relay.image[..]),
        chain.image
    );

    let (own_pid, own_start) = own_anchor();
    assert_eq!(ancestors[1].pid, own_pid, "the grandparent was misread");
    assert_eq!(
        ancestors[1].start, own_start,
        "the grandparent's start was misread"
    );

    // The leaf runs the same image but sends the request, so it is not
    // counted: one image up to this test process.
    let verdict = judge_lineage(
        &lineage,
        Some((own_pid, &own_start)),
        &patterns(&[&chain.image]),
    );
    assert_eq!(verdict_name(&verdict), "accepted");

    let folded = chain.image.to_lowercase();
    let verdict = judge_lineage(&lineage, Some((own_pid, &own_start)), &patterns(&[&folded]));
    let expected = if cfg!(windows) {
        "accepted"
    } else {
        "undecidable"
    };
    assert_eq!(
        verdict_name(&verdict),
        expected,
        "a pattern differing from the image in case alone"
    );
}

#[test]
fn a_real_nested_chain_holds_two_images() {
    use yagura_os::ancestry::{judge_lineage, read_lineage};
    let chain = Chain::start("nested", "relay:1");
    let lineage = read_lineage(chain.leaf);
    let ancestors = lineage.ancestors();
    let (own_pid, own_start) = own_anchor();
    assert!(
        ancestors.len() >= 3,
        "the lineage stopped short of this test process: {} steps",
        ancestors.len()
    );
    assert_eq!(ancestors[1].pid, chain.first.id());
    assert_eq!(ancestors[2].pid, own_pid);
    let verdict = judge_lineage(
        &lineage,
        Some((own_pid, &own_start)),
        &patterns(&[&chain.image]),
    );
    assert_eq!(verdict_name(&verdict), "nested");

    chain.update_file(Update::RenamedAside);
    let verdict = judge_lineage(
        &read_lineage(chain.leaf),
        Some((own_pid, &own_start)),
        &patterns(&[&chain.image]),
    );
    assert_eq!(
        verdict_name(&verdict),
        "nested",
        "a nested CLI whose file was renamed aside"
    );
}

/// A window's CLI that replaced its own executable to update itself still
/// runs under the name it was started with, so a request of its child is
/// still taken.
#[test]
fn a_cli_that_replaced_its_own_file_still_counts_under_the_name_it_started_with() {
    use yagura_os::ancestry::{judge_lineage, read_lineage};
    let mut updates = vec![Update::RenamedAside];
    if cfg!(unix) {
        updates.extend([Update::ReplacedOver, Update::Unlinked]);
    }
    let (own_pid, own_start) = own_anchor();
    let verdicts: Vec<(Update, &str)> = updates
        .into_iter()
        .map(|how| {
            let chain = Chain::start("updated", "relay:0");
            chain.update_file(how);
            let verdict = judge_lineage(
                &read_lineage(chain.leaf),
                Some((own_pid, &own_start)),
                &patterns(&[&chain.image]),
            );
            (how, verdict_name(&verdict))
        })
        .collect();
    assert!(
        verdicts.iter().all(|(_, verdict)| *verdict == "accepted"),
        "{verdicts:?}"
    );
}

/// Unix hands a child whose parent ended to a subreaper or PID 1, so the
/// lineage no longer reaches the process that started the parent.
#[cfg(unix)]
#[test]
fn a_child_whose_parent_ended_no_longer_reaches_the_anchor() {
    use yagura_os::ancestry::{judge_lineage, read_lineage};
    let mut chain = Chain::start("orphan", "orphan");
    let status = chain.first.wait().unwrap();
    assert!(status.success(), "the orphan's parent failed: {status}");
    let lineage = read_lineage(chain.leaf);
    let (own_pid, own_start) = own_anchor();
    assert!(
        lineage.ancestors().iter().all(|step| step.pid != own_pid),
        "an orphan's lineage still passed through the process that started its parent"
    );
    let verdict = judge_lineage(
        &lineage,
        Some((own_pid, &own_start)),
        &patterns(&[&chain.image]),
    );
    assert_eq!(verdict_name(&verdict), "undecidable");
}
