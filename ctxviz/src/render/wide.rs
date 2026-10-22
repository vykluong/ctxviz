//! Wide two-row status line (layout "W1R"): the bar takes every column row 1
//! does not need, and row 2 is the place line with the three largest
//! categories right-aligned. Rows never wrap; both end [`PAD_RIGHT`] cells
//! before the terminal edge.
//!
//! ```text
//! Claude Opus 5.5  ‹300K Medium›  ████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  ~36.4K/300K  11m
//! ~/code/app  git:main  +41 −7                            ■ chat 22.3K  ■ tools 7K  ■ sys 3.3K
//! ```
//!
//! Row 1 keeps its text and gives the rest to the bar. When fewer than
//! [`bar::MIN_WIDTH`] cells are left it drops the `‹badge›`, then shortens the
//! model to its family (`Opus 5.5`), then drops the duration, and finally
//! truncates. Row 2 shortens the path to its leaf, then drops legend entries
//! (three, two, one, none), then truncates.

use crate::render::bar;
use crate::render::statusline::{
    self as sl, compact, percent_color, BarStyle, Span, StatusPayload, Trend, Usage, BRANCH_CLR, PATH_BASE_CLR,
    PATH_LEAF_CLR, TOKEN_CLR, TXT_DIM,
};
use crate::theme::{Role, Theme};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Cells left empty at the right edge of both rows.
pub const PAD_RIGHT: usize = 2;
/// Legend entries on row 2, largest first.
pub const LEGEND_TOP: usize = 3;
/// Longest wait for `git diff --shortstat` before the counts are skipped.
pub const GIT_TIMEOUT: Duration = Duration::from_millis(150);
const SEP: &str = "  ";

/// Both rows, each at most `width` cells wide. With `no_git` row 2 has no
/// branch or diff segment and git is never consulted.
pub fn rows(
    p: &StatusPayload,
    u: &Usage,
    t: &Trend,
    theme: &Theme,
    style: BarStyle,
    width: usize,
    no_git: bool,
) -> [Vec<Span>; 2] {
    [row1(p, u, t, theme, style, width), row2(p, u, width, no_git)]
}

// ── Row 1 ──────────────────────────────────────────────────────────────────

fn row1(p: &StatusPayload, u: &Usage, t: &Trend, theme: &Theme, style: BarStyle, width: usize) -> Vec<Span> {
    let (name, badge) = sl::model_badge(p);
    let family: Vec<Span> = vec![Span::colored(family_name(&text(&name)), sl::MODEL_CLR).bold()];
    let tokens: Vec<Span> = match (u.used, u.max) {
        (Some(used), Some(max)) => vec![Span::colored(format!("~{}/{}", compact(used), compact(max)), TOKEN_CLR)],
        _ => vec![Span::colored("--%", TXT_DIM)],
    };
    let duration: Vec<Span> =
        t.duration_secs.map(|s| vec![Span::colored(duration_text(s), TOKEN_CLR)]).unwrap_or_default();

    // (badge, full name, duration): each config drops one more piece.
    let configs = [(true, true, true), (false, true, true), (false, false, true), (false, false, false)];
    let limit = width.saturating_sub(PAD_RIGHT);
    let mut chosen: Option<(Vec<Span>, Vec<Span>, usize)> = None;
    for (i, &(with_badge, full_name, with_dur)) in configs.iter().enumerate() {
        let mut head: Vec<Span> = if full_name { name.clone() } else { family.clone() };
        if with_badge && !badge.is_empty() {
            head.push(Span::new(SEP, None));
            head.extend(badge.iter().cloned());
        }
        head.push(Span::new(SEP, None));
        let mut tail = vec![Span::new(SEP, None)];
        tail.extend(tokens.iter().cloned());
        if with_dur && !duration.is_empty() {
            tail.push(Span::new(SEP, None));
            tail.extend(duration.iter().cloned());
        }
        let fixed = sl::spans_width(&head) + sl::spans_width(&tail);
        let cells = limit.saturating_sub(fixed);
        if cells >= bar::MIN_WIDTH || i == configs.len() - 1 {
            chosen = Some((head, tail, cells.max(bar::MIN_WIDTH)));
            break;
        }
    }
    let (head, tail, bar_w) = chosen.expect("the last config is always chosen");

    let mut row = head;
    row.push(bar_span(u, bar_w, theme, style));
    row.extend(tail);
    sl::truncate(&mut row, width);
    row
}

/// The bar as one pre-rendered span: category segments, or a single run in
/// the percent color when only the payload is known.
fn bar_span(u: &Usage, bar_w: usize, theme: &Theme, style: BarStyle) -> Span {
    let pct_color = u.percent.map_or(TXT_DIM, percent_color);
    let ansi = if u.categories.is_empty() {
        let cells = match (u.used, u.max, u.percent) {
            (Some(used), Some(max), _) => bar::filled_cells(used, max, bar_w),
            (_, _, Some(pct)) => bar::filled_cells((pct * 100.0).round() as u64, 10_000, bar_w),
            _ => 0,
        };
        let segs = [bar::Segment { category_id: String::new(), cells }];
        bar::ansi_with(&segs, bar_w, theme, |_| theme.paint(pct_color))
    } else {
        let max = u.max.unwrap_or_else(|| u.categories.iter().map(|c| c.1).sum());
        let segs = bar::segments(&u.categories, max, bar_w);
        match style {
            BarStyle::Plain => bar::ansi(&segs, bar_w, theme),
            BarStyle::Labeled => bar::ansi_labeled(&segs, bar_w, theme),
        }
    };
    Span::raw(ansi, bar_w)
}

/// `Claude Opus 5.5` → `Opus 5.5`: the name without its first word, when
/// that leaves something.
pub fn family_name(name: &str) -> String {
    match name.split_once(' ') {
        Some((_, rest)) if !rest.trim().is_empty() => rest.trim().to_string(),
        _ => name.to_string(),
    }
}

/// `11m`, `1h 04m`, `2d 3h`.
pub fn duration_text(secs: u64) -> String {
    let m = secs / 60;
    if m < 60 {
        format!("{m}m")
    } else if m < 24 * 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{}d {}h", m / (24 * 60), (m % (24 * 60)) / 60)
    }
}

// ── Row 2 ──────────────────────────────────────────────────────────────────

fn row2(p: &StatusPayload, u: &Usage, width: usize, no_git: bool) -> Vec<Span> {
    let dir = p
        .work_dir()
        .map(str::to_string)
        .unwrap_or_else(|| std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default());
    let home = std::env::var("HOME").unwrap_or_default();
    let short = sl::shorten_path(&dir, &home);
    let (base, leaf) = match short.rfind('/') {
        Some(i) => (&short[..=i], &short[i + 1..]),
        None => ("", short.as_str()),
    };
    let mut path = Vec::new();
    if !base.is_empty() {
        path.push(Span::colored(base, PATH_BASE_CLR));
    }
    path.push(Span::colored(leaf, PATH_LEAF_CLR).bold());

    let mut git: Vec<Span> = Vec::new();
    if let Some(branch) = (!no_git).then(|| sl::git_branch(Path::new(&dir))).flatten() {
        git.push(Span::new(SEP, None));
        git.push(Span::colored("git:", TXT_DIM));
        git.push(Span::colored(branch, BRANCH_CLR).bold());
        if let Some((add, del)) = git_diff_counts(Path::new(&dir)).filter(|(a, d)| *a > 0 || *d > 0) {
            git.push(Span::new(SEP, None));
            git.push(Span::colored(format!("+{add} −{del}"), TOKEN_CLR));
        }
    }

    let limit = width.saturating_sub(PAD_RIGHT);
    let mut left = path;
    left.extend(git.iter().cloned());
    if sl::spans_width(&left) > limit {
        left = vec![Span::colored(leaf, PATH_LEAF_CLR).bold()];
        left.extend(git);
        if sl::spans_width(&left) > limit {
            sl::truncate(&mut left, width);
            return left;
        }
    }

    let room = limit.saturating_sub(sl::spans_width(&left) + SEP.chars().count());
    let legend = legend(&u.categories, room);
    if legend.is_empty() {
        return left;
    }
    let gap = limit - sl::spans_width(&left) - sl::spans_width(&legend);
    left.push(Span::new(" ".repeat(gap), None));
    left.extend(legend);
    left
}

/// The [`LEGEND_TOP`] largest categories, largest first, as `■ chat 22.3K`
/// entries two spaces apart. When they do not fit in `width`: fewer entries,
/// then nothing.
pub fn legend(cats: &[(String, u64)], width: usize) -> Vec<Span> {
    let mut top: Vec<&(String, u64)> = cats.iter().filter(|c| c.1 > 0).collect();
    top.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| crate::model::categories::order(&a.0).cmp(&crate::model::categories::order(&b.0)))
    });
    top.truncate(LEGEND_TOP);
    if top.is_empty() {
        return Vec::new();
    }
    let build = |n: usize| -> Vec<Span> {
        let mut out = Vec::new();
        for (id, tokens) in top.iter().take(n).map(|c| (&c.0, c.1)) {
            if !out.is_empty() {
                out.push(Span::new(SEP, None));
            }
            out.push(Span::colored("■ ", Role::category(id)));
            out.push(Span::colored(label(id), PATH_LEAF_CLR));
            out.push(Span::colored(format!(" {}", compact(tokens)), TOKEN_CLR));
        }
        out
    };
    (1..=top.len()).rev().map(build).find(|l| sl::spans_width(l) <= width).unwrap_or_default()
}

/// Short legend names for this layout.
pub fn label(id: &str) -> &str {
    match id {
        "system_prompt" => "sys",
        "subagents" => "agents",
        other => crate::model::categories::short_label(other),
    }
}

// ── Git ────────────────────────────────────────────────────────────────────

/// Config overrides on every git call. The workspace is untrusted: its
/// `.git/config` can name programs that git runs during a plain `diff`
/// (fsmonitor daemon, hooks, external diff, pager). Command-line `-c` beats
/// repository config. Textconv drivers are off via `--no-textconv`, and clean
/// filters are neutralized per driver (see [`filter_overrides`]).
pub const GIT_SAFE_CONFIG: &[&str] = &[
    "core.fsmonitor=false",
    "core.hooksPath=/dev/null",
    "diff.external=",
    "core.pager=cat",
    "diff.noprefix=false",
    "core.attributesFile=/dev/null",
];

/// Environment for git, after `env_clear()`. `GIT_CONFIG_GLOBAL=/dev/null`
/// and `GIT_CONFIG_NOSYSTEM=1` also take the user's and the system's config
/// out of play; that drops user aliases and settings, which a diff count
/// does not need.
pub const GIT_ENV: &[(&str, &str)] = &[
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", "/bin/true"),
    ("LC_ALL", "C"),
];

/// Largest git stdout read.
const GIT_OUTPUT_CAP: u64 = 64 * 1024;

/// `git --no-pager -C <dir> -c <safe>...` with a cleared environment (only
/// `PATH`, `HOME` and [`GIT_ENV`]), null stdin and stderr, piped stdout.
fn git_command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env_clear();
    for key in ["PATH", "HOME"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.envs(GIT_ENV.iter().copied());
    cmd.arg("--no-pager").arg("-C").arg(dir);
    for c in GIT_SAFE_CONFIG {
        cmd.arg("-c").arg(c);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    cmd
}

/// Run `cmd` until it exits or `deadline` passes (then it is killed).
/// Returns the exit code and stdout.
fn run_until(mut cmd: Command, deadline: Instant) -> Option<(i32, String)> {
    let mut child = cmd.spawn().ok()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                child.stdout.take()?.take(GIT_OUTPUT_CAP).read_to_string(&mut out).ok()?;
                return Some((status.code()?, out));
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `-c` overrides that turn every configured `filter.<driver>` into a no-op.
/// A diff against the working tree runs a driver's `clean` (or `process`)
/// command on files that `.gitattributes` assigns to it, and both the driver
/// and the attributes come from the repository. None (skip git) when a
/// driver name could not be passed safely as `-c key=value`.
fn filter_overrides(dir: &Path, deadline: Instant) -> Option<Vec<String>> {
    let mut cmd = git_command(dir);
    cmd.args(["config", "-z", "--includes", "--name-only", "--get-regexp", r"^filter\."]);
    let (code, out) = run_until(cmd, deadline)?;
    // Exit 1: no matching keys.
    if code == 1 && out.is_empty() {
        return Some(Vec::new());
    }
    if code != 0 {
        return None;
    }
    let mut drivers: Vec<&str> = Vec::new();
    for key in out.split(['\0', '\n']).filter(|k| !k.is_empty()) {
        let rest = key.strip_prefix("filter.")?;
        let driver = &rest[..rest.rfind('.')?];
        if driver.is_empty() || driver.contains('=') || driver.chars().any(char::is_control) {
            return None;
        }
        if !drivers.contains(&driver) {
            drivers.push(driver);
        }
    }
    Some(
        drivers
            .iter()
            .flat_map(|d| ["clean=", "smudge=", "process=", "required=false"].map(|v| format!("filter.{d}.{v}")))
            .collect(),
    )
}

/// `(insertions, deletions)` of the working tree against `HEAD`, from
/// `git diff --shortstat HEAD` run with [`GIT_SAFE_CONFIG`], [`GIT_ENV`],
/// `--no-ext-diff --no-textconv` and neutralized filters. None when git is
/// missing, a call fails, or the calls take longer than [`GIT_TIMEOUT`] in
/// total (the process is killed).
pub fn git_diff_counts(dir: &Path) -> Option<(u64, u64)> {
    let deadline = Instant::now() + GIT_TIMEOUT;
    let filters = filter_overrides(dir, deadline)?;
    let mut cmd = git_command(dir);
    for f in &filters {
        cmd.arg("-c").arg(f);
    }
    cmd.args(["diff", "--no-ext-diff", "--no-textconv", "--shortstat", "HEAD"]);
    match run_until(cmd, deadline)? {
        (0, out) => Some(parse_shortstat(&out)),
        _ => None,
    }
}

/// `" 3 files changed, 41 insertions(+), 7 deletions(-)"` → `(41, 7)`.
/// An empty line (clean tree) is `(0, 0)`.
pub fn parse_shortstat(s: &str) -> (u64, u64) {
    let mut add = 0;
    let mut del = 0;
    for part in s.split(',') {
        let part = part.trim();
        let n: u64 = part.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if part.contains("insertion") {
            add = n;
        } else if part.contains("deletion") {
            del = n;
        }
    }
    (add, del)
}

fn text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::breakdown::{BreakdownEntry, TokenDetails};
    use crate::render::statusline::{render_themed, strip_ansi, Layout, RenderOpts, Rows};
    use crate::theme::{Family, Mode};

    fn details() -> TokenDetails {
        let c = |id: &str, t| BreakdownEntry { id: id.into(), label: id.into(), estimated_tokens: t };
        TokenDetails {
            used_tokens: 15624,
            max_tokens: 300_000,
            categories: vec![
                c("system_prompt", 3346),
                c("tools", 6354),
                c("rules", 630),
                c("skills", 1884),
                c("mcp", 456),
                c("subagents", 1513),
                c("summarized_conversation", 0),
                c("conversation", 1441),
            ],
        }
    }

    fn payload(width: usize) -> StatusPayload {
        StatusPayload::parse(&format!(
            r#"{{"session_id":"x","cwd":"/tmp","model":{{"display_name":"Claude Opus 5.5 300K Medium","param_summary":"300K Medium"}},
               "context_window":{{"used_percentage":5,"context_window_size":300000}},"render_width_chars":{width}}}"#
        ))
        .unwrap()
    }

    fn trend() -> Trend {
        Trend {
            values: vec![14_000, 15_624],
            compaction: vec![false, false],
            delta: Some(1624),
            duration_secs: Some(11 * 60),
        }
    }

    fn lines(width: usize, td: Option<&TokenDetails>, t: &Trend) -> Vec<String> {
        let theme = Theme::preset(Family::Modus, Mode::Light);
        let out = render_themed(&payload(width), td, t, &theme, RenderOpts::default());
        strip_ansi(&out).lines().map(str::to_string).collect()
    }

    #[test]
    fn default_layout_is_wide() {
        assert_eq!(RenderOpts::default().layout, Layout::Wide);
        assert_eq!(RenderOpts::default().rows, Rows::Two);
    }

    #[test]
    fn rows_fit_and_never_wrap() {
        for w in [12, 20, 30, 40, 48, 60, 64, 80, 90, 100, 118, 140, 200] {
            for td in [None, Some(details())] {
                for t in [Trend::default(), trend()] {
                    let out = lines(w, td.as_ref(), &t);
                    assert_eq!(out.len(), 2, "width {w}: {out:?}");
                    for line in &out {
                        assert!(line.chars().count() <= w, "width {w}: {} chars: {line:?}", line.chars().count());
                    }
                }
            }
        }
    }

    #[test]
    fn row1_bar_absorbs_width() {
        for w in [80, 118, 140, 200] {
            let out = lines(w, Some(&details()), &trend());
            let r1 = &out[0];
            assert!(r1.starts_with("Claude Opus 5.5  ‹300K Medium›  █"), "{r1}");
            assert!(r1.ends_with("  ~15.6K/300K  11m"), "{r1}");
            // Ends PAD_RIGHT short of the width; the bar is the only thing that grew.
            assert_eq!(r1.chars().count(), w - PAD_RIGHT, "{r1}");
            let bar: String = r1.chars().filter(|c| *c == '█' || *c == '░').collect();
            let fixed = "Claude Opus 5.5  ‹300K Medium›  ".chars().count() + "  ~15.6K/300K  11m".chars().count();
            assert_eq!(bar.chars().count(), w - PAD_RIGHT - fixed);
        }
    }

    #[test]
    fn row1_drops_badge_then_family_then_duration() {
        let has = |w: usize| {
            let r1 = lines(w, Some(&details()), &trend()).remove(0);
            (r1.contains('‹'), r1.contains("Claude"), r1.contains("11m"), r1.contains("/300K"))
        };
        let mut prev = (true, true, true, true);
        for w in (12..=200).rev() {
            let cur = has(w);
            let v = [cur.0, cur.1, cur.2, cur.3];
            for i in 1..v.len() {
                assert!(!v[i - 1] || v[i], "width {w}: {cur:?}");
            }
            let p = [prev.0, prev.1, prev.2, prev.3];
            assert!(v.iter().zip(p).all(|(c, p)| !*c || p), "width {w}: {cur:?} after {prev:?}");
            prev = cur;
        }
        assert_eq!(has(200), (true, true, true, true));
        let narrow = lines(48, Some(&details()), &trend()).remove(0);
        assert!(narrow.starts_with("Opus 5.5  █") || narrow.starts_with("Claude Opus 5.5  █"), "{narrow}");
        assert!(!narrow.contains('‹'), "{narrow}");
    }

    #[test]
    fn row2_legend_is_top_three_descending_and_right_aligned() {
        let out = lines(118, Some(&details()), &trend());
        let r2 = &out[1];
        assert!(r2.starts_with("~/tmp") || r2.starts_with("/tmp"), "{r2}");
        assert!(r2.ends_with("■ tools 6.4K  ■ sys 3.3K  ■ skills 1.9K"), "{r2}");
        assert_eq!(r2.chars().count(), 118 - PAD_RIGHT, "{r2}");
        assert_eq!(r2.matches('■').count(), 3, "{r2}");
        assert!(!r2.contains("more"), "{r2}");
        // Payload only: no legend, row ends with the path (or git info).
        let out = lines(118, None, &trend());
        assert!(!out[1].contains('■'), "{}", out[1]);
    }

    #[test]
    fn legend_shrinks_in_order() {
        let cats: Vec<(String, u64)> =
            details().categories.iter().map(|c| (c.id.clone(), c.estimated_tokens)).collect();
        let t = |s: &[Span]| s.iter().map(|x| x.text.as_str()).collect::<String>();
        assert_eq!(t(&legend(&cats, 200)), "■ tools 6.4K  ■ sys 3.3K  ■ skills 1.9K");
        assert_eq!(t(&legend(&cats, 30)), "■ tools 6.4K  ■ sys 3.3K");
        assert_eq!(t(&legend(&cats, 19)), "■ tools 6.4K");
        assert_eq!(t(&legend(&cats, 12)), "■ tools 6.4K");
        assert!(legend(&cats, 11).is_empty());
        assert!(legend(&[], 100).is_empty());
        for w in 0..60 {
            assert!(sl::spans_width(&legend(&cats, w)) <= w, "width {w}");
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(family_name("Claude Opus 5.5"), "Opus 5.5");
        assert_eq!(family_name("Cursor"), "Cursor");
        assert_eq!(family_name("GPT 5"), "5");
        assert_eq!(duration_text(0), "0m");
        assert_eq!(duration_text(11 * 60 + 30), "11m");
        assert_eq!(duration_text(64 * 60), "1h 04m");
        assert_eq!(duration_text(27 * 3600), "1d 3h");
        assert_eq!(parse_shortstat(" 3 files changed, 41 insertions(+), 7 deletions(-)\n"), (41, 7));
        assert_eq!(parse_shortstat(" 1 file changed, 2 deletions(-)\n"), (0, 2));
        assert_eq!(parse_shortstat(""), (0, 0));
        assert_eq!(label("system_prompt"), "sys");
        assert_eq!(label("subagents"), "agents");
        assert_eq!(label("conversation"), "chat");
    }

    #[test]
    fn git_counts_on_this_repo_or_none() {
        // Either a repo (any counts) or not (None); must return within the guard.
        let start = Instant::now();
        let _ = git_diff_counts(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(start.elapsed() < GIT_TIMEOUT + Duration::from_millis(100));
        assert_eq!(git_diff_counts(Path::new("/nonexistent-dir-ctxviz")), None);
    }

    /// A fresh empty dir unique to this test, removed on drop.
    fn temp_dir(tag: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(tag)
    }

    /// Plain git for test setup, isolated from the developer's config.
    fn setup_git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[cfg(unix)]
    #[test]
    fn git_diff_counts_does_not_run_repo_configured_programs() {
        use std::os::unix::fs::PermissionsExt;
        if Command::new("git").arg("--version").stdout(Stdio::null()).status().is_err() {
            eprintln!("skipping: git is not on PATH");
            return;
        }
        let tmp = temp_dir("evil-repo");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(setup_git(&repo, &["init", "-q"]));
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        std::fs::write(repo.join(".gitattributes"), "*.txt filter=evil diff=evil\n").unwrap();
        assert!(setup_git(&repo, &["add", "."]));
        assert!(setup_git(&repo, &["commit", "-q", "-m", "init"]));

        // Every program the repository's config names would touch a marker.
        let marker = |name: &str| tmp.join(format!("pwned-{name}"));
        let script = |name: &str| {
            let path = tmp.join(format!("{name}.sh"));
            std::fs::write(&path, format!("#!/bin/sh\ntouch '{}'\ncat\n", marker(name).display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.display().to_string()
        };
        let hooks = tmp.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for hook in ["pre-command", "post-command", "post-index-change", "reference-transaction", "post-checkout"] {
            let path = hooks.join(hook);
            std::fs::write(&path, format!("#!/bin/sh\ntouch '{}'\n", marker("hook").display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = [
            ("core.fsmonitor", format!("touch '{}'", marker("fsmonitor").display())),
            ("core.hooksPath", hooks.display().to_string()),
            ("diff.external", script("external")),
            ("diff.evil.command", script("diffcmd")),
            ("diff.evil.textconv", script("textconv")),
            ("filter.evil.clean", script("clean")),
            ("filter.evil.smudge", script("smudge")),
            ("core.pager", script("pager")),
        ];
        for (k, v) in &config {
            assert!(setup_git(&repo, &["config", k, v]), "{k}");
        }
        // Dirty, with a new mtime, so git has to read the file through its filter.
        std::fs::write(repo.join("a.txt"), "two\n").unwrap();
        let markers = ["fsmonitor", "hook", "external", "diffcmd", "textconv", "clean", "smudge", "pager"];
        let fired = || markers.iter().filter(|m| marker(m).exists()).copied().collect::<Vec<_>>();

        // Sanity check that the repository really is hostile: a plain diff
        // runs some of these programs (which ones depends on the git version).
        let _ = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["diff", "--shortstat", "HEAD"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let plain = fired();
        eprintln!("plain git diff ran: {plain:?}");
        for m in markers {
            let _ = std::fs::remove_file(marker(m));
        }

        let mut counts = None;
        for _ in 0..3 {
            let start = Instant::now();
            counts = git_diff_counts(&repo);
            assert!(start.elapsed() < GIT_TIMEOUT + Duration::from_millis(100), "{:?}", start.elapsed());
            assert_eq!(fired(), Vec::<&str>::new(), "hardened git ran a repo-configured program");
            if counts.is_some() {
                break;
            }
        }
        assert_eq!(counts, Some((1, 1)));
        assert!(!plain.is_empty(), "the plain diff should have run at least one program");
    }

    #[test]
    fn no_git_skips_branch_and_counts() {
        let dir = env!("CARGO_MANIFEST_DIR");
        let p = StatusPayload::parse(&format!(r#"{{"cwd":"{dir}","render_width_chars":200}}"#)).unwrap();
        let theme = Theme::plain();
        let with = render_themed(&p, None, &Trend::default(), &theme, RenderOpts::default());
        let without =
            render_themed(&p, None, &Trend::default(), &theme, RenderOpts { no_git: true, ..Default::default() });
        if sl::git_branch(Path::new(dir)).is_some() {
            assert!(with.contains("git:"), "{with}");
        }
        assert!(!without.contains("git:"), "{without}");
    }
}
