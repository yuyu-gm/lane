mod artifacts;
mod cleanup;
mod core;
mod interrupt;
mod util;
use core::Store;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, process::Command};
use util::*;

#[derive(Clone)]
struct Arg {
    name: &'static str,
    kind: &'static str,
    default: Value,
    repeat: bool,
    choices: Vec<&'static str>,
}
fn option(name: &'static str, kind: &'static str, default: Value, repeat: bool) -> Arg {
    Arg {
        name,
        kind,
        default,
        repeat,
        choices: vec![],
    }
}
fn flag(name: &'static str) -> Arg {
    option(name, "boolean", json!(false), false)
}
fn textarg(name: &'static str, default: Value) -> Arg {
    option(name, "string", default, false)
}
fn specs() -> BTreeMap<&'static str, (&'static str, Vec<Arg>)> {
    let mut m = BTreeMap::new();
    for name in ["schema", "doctor", "init"] {
        m.insert(name, ("none", vec![]));
    }
    m.insert(
        "spawn",
        (
            "one",
            vec![
                textarg("--base", json!("HEAD")),
                textarg("--branch", Value::Null),
                option("--expected", "string", json!([]), true),
                textarg("--contract", Value::Null),
                textarg("--owner", Value::Null),
                textarg("--campaign", Value::Null),
            ],
        ),
    );
    for name in ["context", "validate", "packet", "integrate"] {
        m.insert(name, ("one", vec![]));
    }
    m.insert(
        "preflight",
        ("one", vec![textarg("--target", json!("HEAD"))]),
    );
    m.insert(
        "replay",
        (
            "one",
            vec![
                textarg("--target", json!("HEAD")),
                textarg("--new-id", Value::Null),
            ],
        ),
    );
    m.insert("plan", ("many", vec![textarg("--target", json!("HEAD"))]));
    m.insert("integrate-all", ("many", vec![]));
    let mut state = option("--state", "string", json!([]), true);
    state.choices = vec![
        "empty",
        "active",
        "dirty",
        "integrated",
        "cleaned",
        "cleanup-partial",
    ];
    m.insert(
        "status",
        (
            "many",
            vec![
                state,
                flag("--active"),
                flag("--unintegrated"),
                flag("--cleanup-pending"),
                textarg("--owner", Value::Null),
                textarg("--campaign", Value::Null),
                flag("--short"),
                option("--limit", "integer", Value::Null, false),
                option("--offset", "integer", json!(0), false),
            ],
        ),
    );
    m.insert(
        "clean",
        (
            "optional",
            vec![
                flag("--all"),
                flag("--delete-branch"),
                flag("--dry-run"),
                flag("--resume"),
                textarg("--collection", Value::Null),
            ],
        ),
    );
    m.insert(
        "artifacts",
        ("one", vec![option("--root", "string", json!([]), true)]),
    );
    m.insert(
        "collect",
        (
            "one",
            vec![
                option("--root", "string", json!([]), true),
                textarg("--dest", Value::Null),
            ],
        ),
    );
    m.insert(
        "contract",
        (
            "one",
            vec![
                textarg("--file", Value::Null),
                flag("--freeze"),
                flag("--unfreeze"),
            ],
        ),
    );
    let mut result = textarg("--result", json!("passed"));
    result.choices = vec!["passed", "failed"];
    m.insert(
        "attest",
        (
            "one",
            vec![
                textarg("--check", Value::Null),
                result,
                textarg("--evidence", json!("")),
            ],
        ),
    );
    m
}
struct Args {
    command: String,
    project: String,
    ids: Vec<String>,
    opts: Value,
    version: bool,
}
impl Args {
    fn val(&self, k: &str) -> String {
        s(&self.opts, k)
    }
    fn yes(&self, k: &str) -> bool {
        self.opts[k] == true
    }
    fn list(&self, k: &str) -> Vec<String> {
        strings(&self.opts[k])
    }
    fn optional(&self, k: &str) -> Option<String> {
        self.opts[k].as_str().map(str::to_owned)
    }
}
fn parse(argv: &[String]) -> Result<Args> {
    let mut i = 0;
    let mut project = ".".to_owned();
    let mut version = false;
    while i < argv.len() {
        match argv[i].as_str() {
            "--version" => version = true,
            "--project" => {
                i += 1;
                project = argv
                    .get(i)
                    .ok_or_else(|| Error::usage("--project requires a value"))?
                    .clone();
            }
            x if x.starts_with("--project=") => project = x[10..].to_owned(),
            x if x.starts_with('-') => {
                return Err(Error::usage(format!("unrecognized argument: {x}")));
            }
            _ => break,
        }
        i += 1;
    }
    if i == argv.len() {
        if version {
            return Ok(Args {
                command: "version".into(),
                project,
                ids: vec![],
                opts: json!({}),
                version,
            });
        }
        return Err(Error::usage("command is required"));
    }
    let command = argv[i].clone();
    i += 1;
    let spec = specs();
    let (count, options) = spec
        .get(command.as_str())
        .ok_or_else(|| Error::usage(format!("unknown command: {command}")))?;
    let mut opts = json!({});
    for a in options {
        opts[a.name] = a.default.clone();
    }
    let mut ids = vec![];
    let mut positionals = false;
    while i < argv.len() {
        let raw = &argv[i];
        if raw == "--" && !positionals {
            positionals = true;
            i += 1;
            continue;
        }
        if raw.starts_with('-') && !positionals {
            let (key, value) = raw
                .split_once('=')
                .map(|(k, v)| (k, Some(v)))
                .unwrap_or((raw, None));
            let a = options
                .iter()
                .find(|a| a.name == key)
                .ok_or_else(|| Error::usage(format!("unrecognized argument: {key}")))?;
            if a.kind == "boolean" {
                if value.is_some() {
                    return Err(Error::usage(format!("{key} does not take a value")));
                }
                opts[key] = json!(true);
            } else {
                let value = if let Some(v) = value {
                    v.to_owned()
                } else {
                    i += 1;
                    argv.get(i)
                        .filter(|s| !s.starts_with("--"))
                        .ok_or_else(|| Error::usage(format!("{key} requires a value")))?
                        .clone()
                };
                if !a.choices.is_empty() && !a.choices.contains(&value.as_str()) {
                    return Err(Error::usage(format!("invalid value for {key}: {value}")));
                }
                if a.kind == "integer" {
                    opts[key] = json!(value.parse::<u64>().map_err(|_| Error::usage(format!(
                        "{key} requires a nonnegative integer"
                    )))?);
                } else if a.repeat {
                    opts[key].as_array_mut().unwrap().push(json!(value));
                } else {
                    opts[key] = json!(value);
                }
            }
        } else {
            ids.push(raw.clone());
        }
        i += 1;
    }
    if (*count == "one" && ids.len() != 1)
        || (*count == "none" && !ids.is_empty())
        || (*count == "optional" && ids.len() > 1)
    {
        return Err(Error::usage(format!(
            "invalid positional arguments for {command}"
        )));
    }
    if command == "clean" {
        if (ids.len() == 1) == (opts["--all"] == true) {
            return Err(Error::usage("clean requires exactly one of <id> and --all"));
        }
        if opts["--all"] == true && !opts["--collection"].is_null() {
            return Err(Error::usage("--collection requires a single lane ID"));
        }
    }
    if command == "collect" && opts["--dest"].is_null() {
        return Err(Error::usage("collect requires --dest"));
    }
    if command == "attest" && opts["--check"].is_null() {
        return Err(Error::usage("attest requires --check"));
    }
    if command == "contract" && opts["--freeze"] == true && opts["--unfreeze"] == true {
        return Err(Error::usage(
            "--freeze and --unfreeze are mutually exclusive",
        ));
    }
    Ok(Args {
        command,
        project,
        ids,
        opts,
        version,
    })
}
fn schema() -> Value {
    let specs = specs();
    let mut defs = json!({});
    for (name, (count, args)) in &specs {
        let arguments:Vec<_>=args.iter().map(|a|json!({"name":a.name,"type":a.kind,"default":a.default,"repeatable":a.repeat,"choices":a.choices,"required":(*name=="collect"&&a.name=="--dest")||(*name=="attest"&&a.name=="--check"),"arity":if a.kind=="boolean"{0}else{1},"value_name":if a.kind=="boolean"{Value::Null}else{json!(a.name.trim_start_matches('-').to_uppercase())}})).collect();
        defs[name] = json!({"positionals":{"name":if *count=="many"{"ids"}else{"id"},"type":"string","min":if *count=="one"{1}else{0},"max":match *count{"one"|"optional"=>json!(1),"none"=>json!(0),_=>Value::Null}},"arguments":arguments});
    }
    defs["clean"]["selection"] = json!({"exactly_one":["id","--all"],"constraints":["--collection cannot be used with --all"]});
    defs["contract"]["mutually_exclusive"] = json!([["--freeze", "--unfreeze"]]);
    json!({"protocol":"lane-agent-response/v1","version":env!("CARGO_PKG_VERSION"),"implementation":"rust","stdout":"single-line-json","agent_execution":"external","exit_codes":{"0":"success","2":"cli_usage_error","10":"invalid_lane_or_changed_path_scope","11":"merge_conflict_or_integration_blocked","12":"lane_operation_error","13":"git_operation_error","130":"interrupted"},"commands":["schema","doctor","init","spawn","context","status","validate","preflight","packet","replay","plan","integrate","integrate-all","clean","artifacts","collect","contract","attest"],"lifecycle":["spawn","external-agent-work","preflight","integrate","clean"],"cleanup":{"usage":"clean (<id> | --all) [--dry-run] [--delete-branch] [--resume] [--collection RECEIPT]","bulk_format":"lane-cleanup-report/v1","statuses":["ready","cleaned","already-cleaned","blocked"],"blocked_exit":12,"history":"retained"},"argument_schema":{"format":"lane-cli-arguments/v1","global_position":"before-command","global_arguments":[{"name":"--project","type":"string","default":".","arity":1},{"name":"--version","type":"boolean","default":false,"arity":0}],"commands":defs},"contract_schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"owner":{"type":"string"},"campaign":{"type":"string"},"symbols":{"type":"array","items":{"type":"string","minLength":1}},"dependencies":{"type":"array","items":{"type":"string","pattern":"^[0-9a-fA-F]{40,64}$"}},"frozen":{"type":"boolean"},"frozen_commit":{"type":"string","pattern":"^[0-9a-fA-F]{40,64}$"},"required_checks":{"type":"array","items":{"type":"string","minLength":1}},"required_artifacts":{"type":"array","items":{"type":"string","minLength":1}}},"additionalProperties":true},"contracts":{"symbols":"cooperative ownership metadata, not semantic edit enforcement","dependencies":"full commit IDs required in lane ancestry","verification":"parent attestations pinned to the current lane tip; no commands executed","frozen":"lane tip must equal frozen_commit","collection":"verified SHA-256 destination and source files required before removing collected files"}})
}
fn check_exit(v: &Value) -> i32 {
    match s(v, "status").as_str() {
        "invalid" => 10,
        "conflict" => 11,
        "blocked" => {
            if v["steps"]
                .as_array()
                .is_some_and(|a| a.iter().any(|s| s["status"] == "invalid"))
            {
                10
            } else {
                11
            }
        }
        _ => 0,
    }
}
fn run(a: &Args) -> Result<(Value, i32)> {
    if a.version {
        return Ok((
            json!({"version":env!("CARGO_PKG_VERSION"),"protocol":"lane-agent-response/v1"}),
            0,
        ));
    }
    if a.command == "schema" {
        return Ok((schema(), 0));
    }
    if a.command == "doctor" {
        let out = Command::new("git").arg("--version").output();
        let repo = Store::from_path(Path::new(&a.project))
            .ok()
            .map(|s| p(&s.repo));
        let git = std::env::var_os("PATH")
            .and_then(|v| {
                std::env::split_paths(&v)
                    .map(|q| q.join(if cfg!(windows) { "git.exe" } else { "git" }))
                    .find(|q| q.is_file())
            })
            .map(|q| p(&q));
        let ok = out.is_ok();
        let mut v = json!({"git":git,"repo":repo,"ok":ok});
        if let Ok(out) = out {
            v["git_version"] = json!(String::from_utf8_lossy(&out.stdout).trim());
        }
        return Ok((v, if ok { 0 } else { 12 }));
    }
    let store = Store::from_path(Path::new(&a.project))?;
    let id = a.ids.first().map(String::as_str).unwrap_or("");
    match a.command.as_str() {
        "init" => {
            store.ensure()?;
            Ok((
                json!({"project":p(&store.repo),"lane_dir":p(&store.root),"config":p(&store.root.join("config.json"))}),
                0,
            ))
        }
        "spawn" => {
            let mut contract = a
                .optional("--contract")
                .map(|q| read_json(Path::new(&q)))
                .transpose()?;
            if a.optional("--owner").is_some() || a.optional("--campaign").is_some() {
                let c = contract.get_or_insert(json!({}));
                for key in ["owner", "campaign"] {
                    if let Some(v) = a.optional(&format!("--{key}")) {
                        c[key] = json!(v);
                    }
                }
            }
            store.spawn(
                id,
                &a.val("--base"),
                a.optional("--branch").as_deref(),
                &a.list("--expected"),
                contract.as_ref(),
            )?;
            Ok((store.context(id)?, 0))
        }
        "context" => Ok((store.context(id)?, 0)),
        "validate" => {
            let v = store.validate(&store.load(id)?)?;
            let code = if v["valid"] == true { 0 } else { 10 };
            Ok((v, code))
        }
        "preflight" => {
            let mut v = store.preflight(id, &a.val("--target"), true)?;
            let code = check_exit(&v);
            v.as_object_mut().unwrap().remove("output");
            Ok((v, code))
        }
        "packet" => {
            let path = store.packet(id, None)?;
            Ok((json!({"path":p(&path),"packet":read_json(&path)?}), 0))
        }
        "plan" => {
            let v = store.plan(&a.ids, &a.val("--target"))?;
            let code = check_exit(&v);
            Ok((v, code))
        }
        "integrate" => {
            store.ensure()?;
            let _lock = store.lock()?;
            Ok((store.integrate_unlocked(id)?, 0))
        }
        "integrate-all" => {
            store.ensure()?;
            let _lock = store.lock()?;
            let ids = if a.ids.is_empty() {
                store.ids(true)?
            } else {
                a.ids.clone()
            };
            let plan = store.plan(&ids, "HEAD")?;
            if plan["status"] != "clean" {
                let code = check_exit(&plan);
                return Ok((json!({"status":"blocked","plan":plan,"receipts":[]}), code));
            }
            let mut receipts = vec![];
            for id in ids {
                receipts.push(store.integrate_unlocked(&id)?);
            }
            Ok((
                json!({"status":"integrated","plan":plan,"receipts":receipts}),
                0,
            ))
        }
        "replay" => {
            let m = store.replay(id, a.optional("--new-id").as_deref(), &a.val("--target"))?;
            Ok((store.context(&s(&m, "id"))?, 0))
        }
        "clean" => {
            let dry = a.yes("--dry-run");
            let delete = a.yes("--delete-branch");
            let resume = a.yes("--resume");
            let collection = a.optional("--collection");
            if a.yes("--all") {
                let v = store.clean_many(delete, dry, resume)?;
                let code = if v["status"] == "blocked" { 12 } else { 0 };
                return Ok((v, code));
            }
            if dry {
                let v =
                    store.cleanup_check(id, delete, collection.as_deref().map(Path::new), resume);
                let code = if v["status"] == "blocked" { 12 } else { 0 };
                Ok((v, code))
            } else {
                store.clean(id, delete, collection.as_deref().map(Path::new), resume)?;
                Ok((store.context(id)?, 0))
            }
        }
        "artifacts" => Ok((store.inventory(id, &a.list("--root"))?, 0)),
        "collect" => Ok((
            store.collect(id, &a.list("--root"), Path::new(&a.val("--dest")))?,
            0,
        )),
        "contract" => {
            let file = a
                .optional("--file")
                .map(|q| read_json(Path::new(&q)))
                .transpose()?;
            let freeze = if a.yes("--freeze") {
                Some(true)
            } else if a.yes("--unfreeze") {
                Some(false)
            } else {
                None
            };
            if file.is_none() && freeze.is_none() {
                Ok((
                    json!({"id":id,"contract":store.load(id)?.get("contract").cloned().unwrap_or(json!({}))}),
                    0,
                ))
            } else {
                Ok((store.set_contract(id, file, freeze)?, 0))
            }
        }
        "attest" => Ok((
            store.attest(
                id,
                &a.val("--check"),
                a.val("--result") == "passed",
                &a.val("--evidence"),
            )?,
            0,
        )),
        "status" => {
            let ids = if a.ids.is_empty() {
                store.ids(false)?
            } else {
                a.ids.clone()
            };
            let mut lanes = vec![];
            let mut status_head = None;
            let filtering = [
                "--state",
                "--active",
                "--unintegrated",
                "--cleanup-pending",
                "--owner",
                "--campaign",
                "--short",
                "--limit",
                "--offset",
            ]
            .iter()
            .any(|k| truth(&a.opts[*k]) && a.opts[*k] != json!(0) && a.opts[*k] != json!([]));
            for id in ids {
                let m = store.load(&id)?;
                if ["owner", "campaign"].iter().any(|k| {
                    a.optional(&format!("--{k}"))
                        .is_some_and(|x| s(&m["contract"], k) != x)
                }) {
                    continue;
                }
                if a.yes("--active") && truth(&m["cleaned_at"]) {
                    continue;
                }
                let mut v = if a.yes("--short") {
                    store.brief_status(&m, &mut status_head)?
                } else {
                    store.status(&id)?
                };
                let state = s(&v, "state");
                if !a.list("--state").is_empty() && !a.list("--state").contains(&state) {
                    continue;
                }
                if a.yes("--active") && !matches!(state.as_str(), "empty" | "active" | "dirty") {
                    continue;
                }
                if a.yes("--unintegrated") && (v["integrated"] == true || state == "cleaned") {
                    continue;
                }
                if a.yes("--cleanup-pending")
                    && (state == "cleaned"
                        || !(v["integrated"] == true
                            || state == "empty"
                            || state == "cleanup-partial"))
                {
                    continue;
                }
                if a.yes("--short") {
                    let tip = v["tip"].clone();
                    v = json!({"id":id,"state":state,"cwd":m["worktree"],"tip":tip,"dirty":v["dirty"],"owner":m["contract"]["owner"]});
                }
                lanes.push(v);
            }
            if filtering {
                let total = lanes.len();
                let offset = a.opts["--offset"].as_u64().unwrap_or(0) as usize;
                let limit = a.opts["--limit"].as_u64().unwrap_or(total as u64) as usize;
                let selected: Vec<_> = lanes.into_iter().skip(offset).take(limit).collect();
                Ok((
                    json!({"lanes":selected,"total":total,"offset":offset,"limit":limit}),
                    0,
                ))
            } else {
                Ok((json!({"lanes":lanes}), 0))
            }
        }
        _ => Err(Error::usage("unhandled command")),
    }
}
fn main() {
    let argv: Vec<_> = std::env::args().skip(1).collect();
    let args = parse(&argv);
    let command = args.as_ref().ok().map(|a| {
        if a.version {
            "version"
        } else {
            a.command.as_str()
        }
    });
    interrupt::install(command);
    let (v, exit) = match args.as_ref() {
        Ok(a) => match run(a) {
            Ok((data, code)) => (
                json!({"format":"lane-agent-response/v1","ok":true,"command":command,"data":data}),
                code,
            ),
            Err(e) => error_envelope(command, e),
        },
        Err(e) => error_envelope(
            command,
            Error {
                exit: e.exit,
                code: e.code,
                message: e.message.clone(),
                details: e.details.clone(),
            },
        ),
    };
    println!("{}", ascii_json(&v));
    std::process::exit(exit)
}
fn error_envelope(command: Option<&str>, e: Error) -> (Value, i32) {
    let mut error = json!({"code":e.code,"message":e.message});
    if !e.details.is_null() {
        error["details"] = e.details;
    }
    (
        json!({"format":"lane-agent-response/v1","ok":false,"command":command,"error":error}),
        e.exit,
    )
}
