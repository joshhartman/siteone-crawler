//! *Copy as PowerShell* (`config::command_line`, `quote_powershell`) and the PowerShell reading of
//! a pasted command line (`config::split_command_line`) against the real PowerShell(s) on this machine: `pwsh`
//! (PowerShell 7, also with `$PSNativeCommandArgumentPassing = 'Legacy'`) and, on Windows,
//! `powershell` (Windows PowerShell 5.1). Each command line runs as pasted input
//! (`[ScriptBlock]::Create`, with `-NoProfile -NonInteractive`), its program replaced by
//!
//! - a PowerShell function that writes each argument it gets (PowerShell's own parsing), and
//! - a native program, built here with `rustc`, that writes its argv — the path the real crawler
//!   (also a Rust program) gets its arguments by.
//!
//! Every argument is written as hex of its UTF-8 to a file, so the console encoding plays no part.
//! The export is for PowerShell 7.3 or later: there every value arrives exactly. Windows
//! PowerShell 5.1 (legacy argument passing) differs exactly for the values
//! `config::powershell_legacy_hazards` names, which the app warns about. Without PowerShell (e.g.
//! on Linux CI) the tests print a skip message and pass.

use std::path::{Path, PathBuf};
use std::process::Command;

use siteone_crawler_gui::model::config::{self, CrawlConfig, Shell, Value};
use siteone_crawler_gui::model::schema::OptionSchema;
use siteone_crawler_gui::model::secrets::SecretValues;

/// A PowerShell to run command lines in.
struct Ps {
    label: String,
    exe: &'static str,
    /// Runs before the command lines (the argument-passing mode).
    prelude: &'static str,
    /// Passes arguments to a program the legacy way (5.1, or 7 in `Legacy` mode).
    legacy: bool,
    /// Windows PowerShell 5.1, whose legacy passing also breaks a trailing backslash.
    windows_powershell: bool,
}

fn version(exe: &str) -> Option<String> {
    let output = Command::new(exe)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The shells found; prints a skip message when there is none.
fn shells() -> Vec<Ps> {
    let mut shells = Vec::new();
    if let Some(version) = version("pwsh") {
        shells.push(Ps {
            label: format!("pwsh {version}"),
            exe: "pwsh",
            prelude: "",
            legacy: false,
            windows_powershell: false,
        });
        // Legacy passing is a Windows matter (elsewhere .NET re-splits the line its own way).
        if cfg!(windows) {
            shells.push(Ps {
                label: format!("pwsh {version} (Legacy passing)"),
                exe: "pwsh",
                prelude: "$PSNativeCommandArgumentPassing = 'Legacy'\n",
                legacy: true,
                windows_powershell: false,
            });
        }
    }
    if cfg!(windows)
        && let Some(version) = version("powershell")
    {
        shells.push(Ps {
            label: format!("powershell {version}"),
            exe: "powershell",
            prelude: "",
            legacy: true,
            windows_powershell: true,
        });
    }
    if shells.is_empty() {
        eprintln!("SKIP: neither pwsh nor powershell was found; the real-shell tests did not run.");
    }
    for ps in &shells {
        eprintln!("testing with {}", ps.label);
    }
    shells
}

/// A PowerShell single-quoted literal (for the test's own script).
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Writes each argument it gets, as the native program does.
const FUNCTION: &str = "function siteone-crawler { [IO.File]::WriteAllLines($global:EchoOut, [string[]]@(foreach ($a in $args) { -join ([Text.Encoding]::UTF8.GetBytes([string]$a) | ForEach-Object { $_.ToString('x2') }) })) }";

/// What the program got from one command line.
#[derive(Debug, PartialEq)]
enum Got {
    Args(Vec<String>),
    /// PowerShell refused or failed the line.
    Error(String),
    /// The program did not run.
    Nothing,
}

/// The native program that writes its argv, `<dir>/bin dir/siteone-crawler(.exe)`.
fn echo_program(dir: &Path) -> PathBuf {
    let source = dir.join("echo_args.rs");
    std::fs::write(
        &source,
        r#"use std::io::Write;
fn main() {
    let path = std::env::var_os("ECHO_ARGS_OUT").expect("ECHO_ARGS_OUT");
    let mut out = std::fs::File::create(path).expect("create");
    for arg in std::env::args_os().skip(1) {
        let arg = arg.into_string().expect("UTF-8");
        let hex: String = arg.bytes().map(|b| format!("{b:02x}")).collect();
        writeln!(out, "{hex}").expect("write");
    }
}
"#,
    )
    .expect("source");
    let bin = dir.join("bin dir");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let exe = bin.join(format!("siteone-crawler{}", std::env::consts::EXE_SUFFIX));
    let next_to_cargo = Path::new(env!("CARGO")).with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    let rustc = if next_to_cargo.is_file() {
        next_to_cargo
    } else {
        PathBuf::from("rustc")
    };
    let status = Command::new(rustc)
        .arg(&source)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("rustc runs");
    assert!(status.success(), "the echo program did not build");
    exe
}

/// Runs each line in `ps` as pasted input, in a script block of its own, with `siteone-crawler`
/// being the function (`function`) or the native program on `PATH`; what each line passed.
fn run(ps: &Ps, dir: &Path, bin: &Path, function: bool, lines: &[String]) -> Vec<Got> {
    let work = dir.join("work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    for (i, line) in lines.iter().enumerate() {
        std::fs::write(work.join(format!("line-{i}.ps1")), line).expect("line");
    }
    let script = format!(
        "$ErrorActionPreference = 'Stop'
{prelude}{function}
$env:Path = {bin} + [IO.Path]::PathSeparator + $env:Path
Set-Location -LiteralPath {work}
foreach ($i in 0..{last}) {{
  $global:EchoOut = Join-Path {work} \"out-$i.txt\"; $env:ECHO_ARGS_OUT = $global:EchoOut
  try {{ . ([ScriptBlock]::Create([IO.File]::ReadAllText((Join-Path {work} \"line-$i.ps1\"), [Text.Encoding]::UTF8))) }}
  catch {{ [IO.File]::WriteAllText((Join-Path {work} \"err-$i.txt\"), $_.ToString()) }}
}}
",
        prelude = ps.prelude,
        function = if function { FUNCTION } else { "" },
        bin = literal(&bin.display().to_string()),
        work = literal(&work.display().to_string()),
        last = lines.len() - 1,
    );
    let file = dir.join("run.ps1");
    std::fs::write(&file, script).expect("script");
    // Read as UTF-8 by both shells (5.1 reads a script file without a BOM as ANSI), and no
    // execution policy applies to a script block.
    let boot = format!(
        ". ([ScriptBlock]::Create([IO.File]::ReadAllText({}, [Text.Encoding]::UTF8)))",
        literal(&file.display().to_string())
    );
    let utf16: Vec<u8> = boot.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let output = Command::new(ps.exe)
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &base64(&utf16)])
        .output()
        .expect("the shell runs");
    assert!(
        output.status.success(),
        "{}: {}\n{}",
        ps.label,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (0..lines.len())
        .map(|i| {
            if let Ok(error) = std::fs::read_to_string(work.join(format!("err-{i}.txt"))) {
                Got::Error(error)
            } else if let Ok(text) = std::fs::read_to_string(work.join(format!("out-{i}.txt"))) {
                Got::Args(text.lines().map(unhex).collect())
            } else {
                Got::Nothing
            }
        })
        .collect()
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unhex(line: &str) -> String {
    let bytes: Vec<u8> = (0..line.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&line[i..i + 2], 16).expect("hex"))
        .collect();
    String::from_utf8(bytes).expect("UTF-8")
}

/// Values that are hard to quote.
const VALUES: &[&str] = &[
    r"C:\Exports\site",
    r"C:\My Exports\site",
    r"\\server\share\dir",
    r"\\server\share\my dir\",
    r"C:\My Exports\",
    r"C:\Exports\",
    "two  spaces and\ttab",
    "  leading and trailing  ",
    "Sam's site",
    "'",
    "''",
    "it\u{2019}s \u{2018}typographic\u{2019} \u{201a}low\u{201b}",
    "\"double\" quotes",
    "say \"hi\"",
    "a\"b",
    "\"",
    "`backtick` and `n `0 `e",
    "$literal $HOME ${x} $(y) @(z) cost: 5$",
    "#not-a-comment",
    "a # b",
    "a;b|c&d(e)f{g}h@i,j%k^l!m~n*o?p<q>r[s]",
    "@splat",
    ",comma",
    "100% %PATH% %domain%",
    "",
    " ",
    "\u{fc}n\u{ef}c\u{f8}d\u{e9} \u{2013} \u{201c}quoted\u{201d} \u{201e}low\u{201c} \u{2603}",
    "-starts-with-dash",
    "-t=5.5",
    "-x:y",
    "--double-dash.value",
    "\u{2013}en-dash.value",
    "line\nbreak",
    "line\r\nbreak",
    "https://example.com/?a=1&b=2#frag",
];

/// The command lines exported for `argv` sets, each closed by a sentinel argument.
fn exported(program: &str, cases: &[Vec<String>]) -> Vec<String> {
    cases
        .iter()
        .map(|argv| {
            let mut argv = argv.clone();
            argv.push("--end".to_string());
            config::command_line(program, &argv, Shell::PowerShell)
        })
        .collect()
}

#[test]
fn exported_command_lines_pass_their_argv_in_real_powershell() {
    let shells = shells();
    if shells.is_empty() {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let echo = echo_program(dir.path());
    let bin = echo.parent().expect("bin dir").to_path_buf();

    // Each value as a scalar option, as a list item (`--name:=value`) and alone.
    let mut cases: Vec<Vec<String>> = VALUES
        .iter()
        .flat_map(|value| {
            [
                vec![format!("--user-agent={value}")],
                vec![format!("--header:=X-Test: {value}")],
                vec![value.to_string()],
            ]
        })
        .collect();
    // A configuration as the app exports it.
    let schema = OptionSchema::load();
    let mut crawl = CrawlConfig::new("https://example.com/?a=1&b=2");
    for (name, value) in [
        ("--offline-export-dir", Value::Text(r"C:\My Exports\site".to_string())),
        ("--user-agent", Value::Text("Sam's \"bot\" $x `y".to_string())),
        (
            "--header",
            Value::List(vec!["X-A: a, b".to_string(), "X-B: it\u{2019}s".to_string()]),
        ),
        ("--workers", Value::Text("3".to_string())),
    ] {
        crawl.set(&schema, name, value).expect(name);
    }
    let app_argv = crawl.to_preview_argv(&schema, &SecretValues::default(), false);
    cases.push(app_argv.clone());

    for function in [true, false] {
        let program = if function {
            config::PROGRAM_NAME.to_string()
        } else {
            // A path with a space: called with `&`.
            echo.display().to_string()
        };
        let mut lines = exported(&program, &cases);
        // The same configuration with every argument on a line of its own.
        let multiline = std::iter::once(program.as_str())
            .chain(app_argv.iter().map(String::as_str))
            .chain(std::iter::once("--end"))
            .map(|arg| config::quote_powershell(arg).into_owned())
            .collect::<Vec<_>>()
            .join(" `\n  ");
        let multiline = if function { multiline } else { format!("& {multiline}") };
        lines.push(multiline);
        let mut expected = cases.clone();
        expected.push(app_argv.clone());

        for ps in &shells {
            let got = run(ps, dir.path(), &bin, function, &lines);
            let mut failures = Vec::new();
            for ((line, argv), got) in lines.iter().zip(&expected).zip(got) {
                let mut want = argv.clone();
                want.push("--end".to_string());
                let survived = got == Got::Args(want);
                let hazards = config::powershell_legacy_hazards(argv);
                let ok = if function || !ps.legacy {
                    survived
                } else if ps.windows_powershell {
                    // Exactly the values the app warns about are passed differently.
                    survived == hazards.is_empty()
                } else {
                    survived || !hazards.is_empty()
                };
                if !ok {
                    failures.push(format!("{line}\n    hazards {hazards:?} -> {got:?}"));
                }
            }
            let mode = if function { "function" } else { "native program" };
            assert!(failures.is_empty(), "{} ({mode}):\n{}", ps.label, failures.join("\n"));
        }
    }
}

/// Hand-written command lines a user may paste, and whether the import refuses them. What the
/// import accepts is exactly what PowerShell passes to the program; what it cannot read so is
/// refused with the reason (`split_powershell`).
const PASTED: &[(&str, bool)] = &[
    // Quoting and escapes.
    (
        r#"siteone-crawler "--user-agent=say `"hi`"" '--x=it''s' --y="a""b""#,
        false,
    ),
    (r#"siteone-crawler 'a""b' "a''b" 'a`b' "a'b""#, false),
    (
        "siteone-crawler \"a`tb\" \"a`nb\" \"a`$b\" \"a``b\" \"a`'b\" \"a`zb\" \"a`fb`vc`ad`be`rf\"",
        false,
    ),
    ("siteone-crawler a`nb a`tb a`$b a`'b a`zb a`,b a`#b", false),
    ("siteone-crawler --a=x'y z'w --b=x\"y z\"w", false),
    (
        "siteone-crawler 'a'b \"a\"'b' 'a'\"b\" a'b'\"c\"d 'a''b'c \"a\"b'c'd",
        false,
    ),
    (
        "siteone-crawler \u{201c}a\u{201d}b \u{2018}a\u{2019}b a\u{201c}b c\u{201d}",
        false,
    ),
    (
        "siteone-crawler --a=\u{201c}x y\u{201d} --b=\u{2018}p q\u{2019} --c=\u{2018}it\u{2019}\u{2019}s\u{2019}",
        false,
    ),
    ("siteone-crawler \"\" '' --a", false),
    (
        "siteone-crawler --a=C:\\x\\ --b=\"C:\\y z\\\\\" --c='C:\\My Exports\\'",
        false,
    ),
    ("siteone-crawler --a=\"multi\nline\" 'a\rb'", false),
    ("siteone-crawler \"$\" \"a$ b\" \"a$\" --a=$ --b=5$ --c=a$", false),
    ("siteone-crawler '--a=$x' --b=`$x \"--c=`$(x)\"", false),
    // Comments, continuation lines, blank lines.
    ("siteone-crawler --url=https://example.com/ # comment here", false),
    ("siteone-crawler --url=https://example.com/#frag a#b `#c #d", false),
    ("siteone-crawler <#c#> --a --b=c<#d#> <# e\n f #> --g", false),
    ("siteone-crawler --a=1 `\n  --b=2 `\n  --c=3", false),
    ("siteone-crawler --a=1 `\r\n  --b=2", false),
    ("siteone-crawler a`\nb", false),
    ("\n# comment\n  siteone-crawler --a=b\n\n# end\n", false),
    // Unusual but literal characters.
    (
        "siteone-crawler --a=b,c a=1,b=2 x2>b --c=b>c --d=b<c [b] a[b] a@b",
        false,
    ),
    (
        "siteone-crawler --a=!b --c=~d --e=*f? --g=^h --i=%PATH% --j=100%",
        false,
    ),
    ("siteone-crawler 1e3 0x10 007 1kb -1.5 -- --a", false),
    (
        "siteone-crawler a\u{a0}b a\u{2003}b a\u{c}b a\u{b}b a\u{85}b a\u{3000}b a\u{2028}b",
        false,
    ),
    ("siteone-crawler --a=x\u{2014}y \u{2013}b", false),
    // Single-dash options.
    ("siteone-crawler -u https://example.com/ -w 3 -H 'X-A: b c'", false),
    ("siteone-crawler -H:X-A:b -a:b.c -a:'b c' -a:\"b.c\" -a: b -a= -", false),
    (
        "siteone-crawler -H'X: a' -u\"https://x.y\" -u\"x.y\".z -a'b'.c -a'b.c' -.a",
        false,
    ),
    (
        "siteone-crawler -a=b/c -b=C:\\x -c_d -e?f -g!h -i%j -k+l -m~n -o*p -q^r -a#b -a@b -a$ -a<b -a>b",
        false,
    ),
    // The program.
    ("siteone-crawler.exe --a=1", false),
    ("& siteone-crawler --a=1", false),
    ("& 'siteone-crawler' --a=1", false),
    // A colon ends a parameter name: the dot after it does not split.
    ("siteone-crawler -u=https://example.com/ -w=3", false),
    // Refused: PowerShell would pass something else, or nothing.
    ("siteone-crawler -u=example.com", true),
    ("siteone-crawler -t=5.5", true),
    ("siteone-crawler -a[b]", true),
    ("siteone-crawler -a1.5", true),
    ("siteone-crawler -a`.b", true),
    ("siteone-crawler \u{2013}a.b", true),
    ("siteone-crawler -_a.b", true),
    ("siteone-crawler -a,b", true),
    ("siteone-crawler --url=https://example.com/?a=1&b=2", true),
    ("siteone-crawler --a=b;c", true),
    ("siteone-crawler --a=b|c", true),
    ("siteone-crawler --a=(b)", true),
    ("siteone-crawler --a={b}", true),
    ("siteone-crawler --a=@(1)", true),
    ("siteone-crawler @a", true),
    ("siteone-crawler @'\nx\n'@", true),
    ("siteone-crawler a >b", true),
    ("siteone-crawler 2>b a", true),
    ("siteone-crawler a <b", true),
    ("siteone-crawler b, c", true),
    ("siteone-crawler --a=b, c", true),
    ("siteone-crawler x ,y", true),
    ("siteone-crawler a,'b'", true),
    ("siteone-crawler 'a',b", true),
    ("siteone-crawler a,,b", true),
    ("siteone-crawler \"a`0b\"", true),
    ("siteone-crawler a`0b", true),
    ("siteone-crawler \"a`e\"", true),
    ("siteone-crawler \"b`u{41}\"", true),
    ("siteone-crawler a`ub", true),
    ("siteone-crawler --% --a=\"x y\" 'z'", true),
    ("siteone-crawler --a=1\n--b=2", true),
    ("siteone-crawler --a=1 ` \n --b=2", true),
    ("siteone-crawler --a=b # c `\n--d", true),
    ("siteone-crawler a\rb", true),
    ("siteone-crawler --a=1 `\n", true),
    ("siteone-crawler --a=b`", true),
    ("siteone-crawler 'open", true),
    ("siteone-crawler <# open", true),
    ("siteone-crawler --url=$url", true),
    ("siteone-crawler \"--url=$($x)\"", true),
    ("siteone-crawler --a=\"${x}\"", true),
    ("siteone-crawler \"$env:KEY\"", true),
    ("siteone-crawler -$a", true),
];

#[test]
fn pasted_command_lines_import_what_real_powershell_passes() {
    // Refusals are pure: checked on every platform.
    for (line, refused) in PASTED {
        let split = config::split_command_line(line, Shell::PowerShell);
        assert_eq!(split.is_err(), *refused, "{line:?}: {split:?}");
    }
    let shells = shells();
    if shells.is_empty() {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let echo = echo_program(dir.path());
    let bin = echo.parent().expect("bin dir").to_path_buf();
    let lines: Vec<String> = PASTED.iter().map(|(line, _)| line.to_string()).collect();
    for ps in &shells {
        let got = run(ps, dir.path(), &bin, false, &lines);
        let mut failures = Vec::new();
        for (line, got) in lines.iter().zip(got) {
            let Ok(split) = config::split_command_line(line, Shell::PowerShell) else {
                continue;
            };
            // The arguments after the program (and the call operator).
            let program_at = usize::from(split.first().is_some_and(|arg| arg == "&"));
            let imported = split[program_at + 1..].to_vec();
            let same = got == Got::Args(imported.clone());
            // Legacy passing changes the values the export warns about; the import reads the
            // command as PowerShell 7.3+ passes it (the README says so).
            let legacy_difference = ps.legacy && !config::powershell_legacy_hazards(&imported).is_empty();
            if !same && !legacy_difference {
                failures.push(format!("{line:?}\n    imported {imported:?}\n    passed   {got:?}"));
            }
        }
        assert!(failures.is_empty(), "{}:\n{}", ps.label, failures.join("\n"));
    }
}
