//! alto-git: Git Adapter and diff tools for Alto packages.
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

#[cfg(not(target_arch = "wasm32"))]
const USAGE: &str = "alto-git — Alto の版管理を Git へ写すツール

使い方:
  alto-git sync <folder.altoproj> [--branch <name>]   Alto のコミットを git コミットへ写す
  alto-git log <package>                               Alto のコミット履歴と変更をレイヤー単位で表示
  alto-git diff <a> <b>                                2つの版をレイヤー単位で比較
  alto-git diff <folder.altoproj>                      作業中の文書と最新コミットを比較
  alto-git export <file.alto> <folder.altoproj>        単一ファイルをフォルダ形式へ展開
  alto-git pack <folder.altoproj> <file.alto>          フォルダ形式を単一ファイルへまとめる
  alto-git textconv <document.json>                    git diff 用の読みやすい文書表示
  alto-git install [<repo>]                            git diff に textconv を登録する

<package> と <a>/<b> には .alto ファイル、フォルダ、または <git-rev>:<folder> を指定できます。";

#[cfg(not(target_arch = "wasm32"))]
fn run(args: &[String]) -> alto_git::Result<String> {
    let arg = |i: usize| {
        args.get(i)
            .map(String::as_str)
            .ok_or_else(|| USAGE.to_string())
    };
    match args.first().map(String::as_str) {
        Some("sync") => {
            let branch = args
                .iter()
                .position(|a| a == "--branch")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str);
            let r = alto_git::sync(Path::new(arg(1)?), branch)?;
            if r.created.is_empty() {
                return Ok(format!("{} は最新です\n", r.branch));
            }
            let mut s = format!(
                "{} に {} 件のコミットを作成しました\n",
                r.branch,
                r.created.len()
            );
            for (h, m) in r.created {
                s += &format!("  {}  {m}\n", &h.hex()[..10]);
            }
            Ok(s)
        }
        Some("log") => alto_git::log_text(&alto_git::Opened::open(arg(1)?)?),
        Some("diff") if args.len() == 2 => {
            let p = alto_git::Opened::open(arg(1)?)?;
            let head = p
                .package
                .head
                .and_then(|h| p.commits.get(&h))
                .ok_or("コミットがありません")?;
            let base = p.snapshot_doc(&head.snapshot)?;
            Ok(alto_git::diff_text(&base, &p.doc))
        }
        Some("diff") => {
            let (a, b) = (
                alto_git::Opened::open(arg(1)?)?,
                alto_git::Opened::open(arg(2)?)?,
            );
            Ok(alto_git::diff_text(&a.doc, &b.doc))
        }
        Some("export") => {
            let r = alto_git::export(Path::new(arg(1)?), Path::new(arg(2)?))?;
            Ok(format!(
                "{} ファイルを書き込み、{} ファイルは既存のため省略しました\n",
                r.written, r.skipped
            ))
        }
        Some("pack") => {
            alto_git::pack(Path::new(arg(1)?), Path::new(arg(2)?))?;
            Ok(String::new())
        }
        Some("textconv") => alto_git::textconv(&std::fs::read(arg(1)?).map_err(|e| e.to_string())?),
        Some("install") => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            alto_git::install(
                Path::new(args.get(1).map_or(".", String::as_str)),
                &exe.to_string_lossy(),
            )?;
            Ok("diff.alto.textconv を登録しました\n".into())
        }
        _ => Err(USAGE.into()),
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(s) => print!("{s}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
