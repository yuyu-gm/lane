use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub struct Error {
    pub exit: i32,
    pub code: &'static str,
    pub message: String,
    pub details: Value,
}
impl Error {
    pub fn lane(message: impl Into<String>) -> Self {
        Self {
            exit: 12,
            code: "lane_error",
            message: message.into(),
            details: Value::Null,
        }
    }
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            exit: 2,
            code: "cli_usage_error",
            message: message.into(),
            details: Value::Null,
        }
    }
    pub fn blocked(id: &str, status: &str) -> Self {
        Self {
            exit: if status == "invalid" { 10 } else { 11 },
            code: "blocked",
            message: format!("lane {id} is blocked: {status}"),
            details: json!({"lane":id,"status":status}),
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::lane(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::lane(e.to_string())
    }
}
pub fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_owned()
}
pub fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
pub fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
pub fn p(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
pub fn absolute(path: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            _ => out.push(c),
        }
    }
    if out.exists() {
        let canon = fs::canonicalize(&out)?;
        #[cfg(windows)]
        {
            return Ok(PathBuf::from(p(&canon).trim_start_matches(r"\\?\")));
        }
        #[cfg(not(windows))]
        {
            return Ok(canon);
        }
    }
    Ok(out)
}
pub fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}
static SERIAL: AtomicU64 = AtomicU64::new(0);
pub fn stamp() -> String {
    format!(
        "{}-{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S-%6f"),
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}
pub fn safe_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(Error::lane(
            "lane id must match [A-Za-z0-9][A-Za-z0-9._-]{0,63}",
        ));
    }
    Ok(())
}
pub fn read_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path)
        .map_err(|e| Error::lane(format!("cannot read {}: {e}", p(path))))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| Error::lane(format!("invalid JSON: {}: {e}", p(path))))?;
    if !v.is_object() {
        return Err(Error::lane(format!("expected JSON object: {}", p(path))));
    }
    Ok(v)
}
pub fn write_json(path: &Path, v: &Value) -> Result<()> {
    fs::create_dir_all(path.parent().ok_or_else(|| Error::lane("missing parent"))?)?;
    let temp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        stamp()
    ));
    let result = (|| {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        f.write_all(serde_json::to_string_pretty(v)?.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if temp.exists() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub fn reparse(path: &Path) -> Result<bool> {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Ok(m.file_attributes() & 0x400 != 0)
    }
    #[cfg(not(windows))]
    {
        Ok(m.file_type().is_symlink())
    }
}
pub fn no_links(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for c in path.components() {
        current.push(c);
        if reparse(&current)? {
            return Err(Error::lane(format!(
                "refusing cleanup: symlink/junction in managed path: {}",
                p(&current)
            )));
        }
    }
    Ok(())
}
pub fn walk(path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = vec![];
    if reparse(path)? {
        return Err(Error::lane(format!(
            "refusing cleanup: symlink/junction entry exists: {}",
            p(path)
        )));
    }
    for item in fs::read_dir(path)? {
        let entry = item?;
        let q = entry.path();
        if reparse(&q)? {
            return Err(Error::lane(format!(
                "refusing cleanup: symlink/junction entry exists: {}",
                p(&q)
            )));
        }
        if entry.file_type()?.is_dir() {
            files.extend(walk(&q)?);
        } else {
            files.push(q);
        }
    }
    Ok(files)
}
pub fn hash(path: &Path) -> Result<(u64, String)> {
    no_links(path)?;
    let mut f = File::open(path)?;
    let mut sha = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut size = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        sha.update(&buf[..n]);
    }
    Ok((size, format!("{:x}", sha.finalize())))
}
pub struct Lock(pub File);
impl Lock {
    pub fn acquire(path: &Path) -> Result<Self> {
        no_links(path)?;
        fs::create_dir_all(path.parent().unwrap())?;
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if f.metadata()?.len() == 0 {
            f.write_all(b"\0")?;
        }
        f.try_lock()
            .map_err(|_| Error::lane(format!("operation lock busy: {}", p(path))))?;
        Ok(Self(f))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
pub struct Output {
    pub code: i32,
    pub out: String,
    pub err: String,
}
pub fn git(repo: &Path, args: &[&str], check: bool) -> Result<Output> {
    git_input(repo, args, check, None, false)
}
pub fn git_input(
    repo: &Path,
    args: &[&str],
    check: bool,
    input: Option<&str>,
    plan: bool,
) -> Result<Output> {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(repo)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    if input.is_some() {
        c.stdin(Stdio::piped());
    } else {
        c.stdin(Stdio::null());
    }
    if plan {
        for k in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
            c.env(k, "lane-plan");
        }
        for k in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
            c.env(k, "lane-plan@local.invalid");
        }
    }
    let mut child = c.spawn()?;
    if let Some(text) = input {
        child.stdin.take().unwrap().write_all(text.as_bytes())?;
    }
    let o = child.wait_with_output()?;
    let result = Output {
        code: o.status.code().unwrap_or(130),
        out: String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        err: String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n"),
    };
    if check && result.code != 0 {
        return Err(git_error(repo, args, &result));
    }
    Ok(result)
}
pub fn git_error(repo: &Path, args: &[&str], o: &Output) -> Error {
    let detail = if o.err.is_empty() { &o.out } else { &o.err };
    Error {
        exit: 13,
        code: "git_error",
        message: format!(
            "git command failed ({}): git -C {} {}\n{}",
            o.code,
            p(repo),
            args.join(" "),
            detail.trim()
        ),
        details: json!({"returncode":o.code}),
    }
}
pub fn resolve(repo: &Path, r: &str) -> Result<String> {
    let out = git(
        repo,
        &["rev-parse", "--verify", &format!("{r}^{{commit}}")],
        true,
    )?
    .out
    .trim()
    .to_owned();
    if !oid(&out) {
        return Err(Error::lane(format!("could not resolve commit: {r}")));
    }
    Ok(out)
}
pub fn oid(s: &str) -> bool {
    (40..=64).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_hexdigit())
}
pub fn branch(repo: &Path) -> Result<String> {
    Ok(git(repo, &["branch", "--show-current"], true)?
        .out
        .trim()
        .to_owned())
}
pub fn ancestor(repo: &Path, a: &str, b: &str) -> Result<bool> {
    Ok(git(repo, &["merge-base", "--is-ancestor", a, b], false)?.code == 0)
}
pub fn git_path(repo: &Path, name: &str) -> Result<PathBuf> {
    let q = PathBuf::from(
        git(repo, &["rev-parse", "--git-path", name], true)?
            .out
            .trim(),
    );
    absolute(&if q.is_absolute() { q } else { repo.join(q) })
}
pub fn changed(repo: &Path, a: &str, b: &str) -> Result<Vec<String>> {
    Ok(git(
        repo,
        &["diff", "--name-only", "--no-renames", "-z", a, b, "--"],
        true,
    )?
    .out
    .split('\0')
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect())
}
pub fn normalize(s: &str) -> String {
    s.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned()
}
pub fn allowed(path: &str, patterns: &[String]) -> bool {
    let v = path.replace('\\', "/");
    let v = v.trim_start_matches("./");
    let patterns: Vec<String> = patterns
        .iter()
        .map(|p| normalize(p))
        .filter(|p| !p.is_empty())
        .collect();
    if patterns.is_empty() {
        return true;
    }
    patterns.iter().any(|p| {
        if p.contains(['*', '?', '[']) {
            fnmatch(v, p)
        } else {
            v == p || v.starts_with(&(p.clone() + "/"))
        }
    })
}
// Python fnmatchcase semantics, including '/', Unicode and invalid/empty ranges.
// Dynamic programming avoids a regex runtime and exponential wildcard backtracking.
pub fn fnmatch(v: &str, pattern: &str) -> bool {
    enum Token {
        Star,
        Any,
        Literal(char),
        Class(bool, Vec<(char, char)>),
    }
    let chars: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = v.chars().collect();
    let mut tokens = vec![];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' => tokens.push(Token::Star),
            '?' => tokens.push(Token::Any),
            '[' => {
                let mut j = i;
                if j < chars.len() && chars[j] == '!' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j == chars.len() {
                    tokens.push(Token::Literal('['));
                    continue;
                }
                let negative = chars[i] == '!';
                let raw = &chars[i + usize::from(negative)..j];
                let mut chunks: Vec<Vec<char>> = vec![];
                let mut start = 0;
                let mut k = 1;
                while k < raw.len() {
                    if let Some(pos) = raw[k..].iter().position(|c| *c == '-') {
                        let end = k + pos;
                        chunks.push(raw[start..end].to_vec());
                        start = end + 1;
                        k = end + 3;
                    } else {
                        break;
                    }
                }
                if start < raw.len() {
                    chunks.push(raw[start..].to_vec());
                } else if let Some(last) = chunks.last_mut() {
                    last.push('-');
                }
                for n in (1..chunks.len()).rev() {
                    if chunks[n - 1].last() > chunks[n].first() {
                        let tail = chunks.remove(n);
                        chunks[n - 1].pop();
                        chunks[n - 1].extend(tail.into_iter().skip(1));
                    }
                }
                let mut stream = vec![];
                for (n, chunk) in chunks.iter().enumerate() {
                    if n > 0 {
                        stream.push(('-', false));
                    }
                    stream.extend(chunk.iter().map(|c| (*c, true)));
                }
                let mut ranges = vec![];
                let mut n = 0;
                while n < stream.len() {
                    if n + 2 < stream.len() && stream[n + 1] == ('-', false) {
                        ranges.push((stream[n].0, stream[n + 2].0));
                        n += 3;
                    } else {
                        ranges.push((stream[n].0, stream[n].0));
                        n += 1;
                    }
                }
                tokens.push(Token::Class(negative, ranges));
                i = j + 1;
            }
            x => tokens.push(Token::Literal(x)),
        }
    }
    let mut dp = vec![false; value.len() + 1];
    dp[0] = true;
    for token in tokens {
        let mut next = vec![false; value.len() + 1];
        match token {
            Token::Star => {
                let mut reached = false;
                for n in 0..dp.len() {
                    reached |= dp[n];
                    next[n] = reached;
                }
            }
            _ => {
                for n in 0..value.len() {
                    let matches = match &token {
                        Token::Any => true,
                        Token::Literal(c) => *c == value[n],
                        Token::Class(negative, ranges) => {
                            ranges.iter().any(|(a, b)| *a <= value[n] && value[n] <= *b)
                                != *negative
                        }
                        _ => false,
                    };
                    next[n + 1] = dp[n] && matches;
                }
            }
        }
        dp = next;
    }
    dp[value.len()]
}
pub fn merge_tree(repo: &Path, a: &str, b: &str) -> Result<Value> {
    let args = ["merge-tree", "--write-tree", "--messages", a, b];
    let o = git(repo, &args, false)?;
    if o.code > 1 {
        return Err(git_error(repo, &args, &o));
    }
    let output = format!("{}{}", o.out, o.err).trim().to_owned();
    let mut paths = vec![];
    for line in output.lines() {
        let pieces: Vec<&str> = line.splitn(2, '\t').collect();
        let path = if pieces.len() == 2 && pieces[0].split_whitespace().count() == 3 {
            Some(pieces[1])
        } else if line.to_ascii_lowercase().contains("conflict (") {
            line.split_once(" in ")
                .map(|(_, s)| s.trim_end_matches('.'))
        } else {
            None
        };
        if let Some(path) = path {
            let path = path.trim().replace('\\', "/");
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    Ok(
        json!({"status":if o.code==1||!paths.is_empty(){"conflict"}else{"clean"},"merged_tree":o.out.lines().find(|s|oid(s.trim())),"conflict_paths":paths,"output":output.chars().take(20000).collect::<String>()}),
    )
}
pub fn ascii_json(v: &Value) -> String {
    let text = v.to_string();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for n in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{n:04x}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_fnmatch_compatibility() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/fixtures/scope_cases.json")).unwrap();
        for case in cases.as_array().unwrap() {
            assert_eq!(
                fnmatch(case[0].as_str().unwrap(), case[1].as_str().unwrap()),
                case[2].as_bool().unwrap(),
                "{case}"
            );
        }
    }
    #[test]
    fn ascii_protocol_handles_surrogate_pairs() {
        assert_eq!(
            ascii_json(&json!("😀日本語")),
            "\"\\ud83d\\ude00\\u65e5\\u672c\\u8a9e\""
        );
    }
}
