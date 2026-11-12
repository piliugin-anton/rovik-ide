use super::{args, checked, read, run, write_json};
use crate::{
    normalizer::Inventory,
    packs,
    profile::{Language, Profile},
    require, Result,
};
use std::path::Path;

pub(super) fn quota() -> Result<i32> {
    let profile: Profile = read("/oracle/profile.json")?;
    packs::c_family_reference_command(&profile)?;
    let config = &profile.configurations[0];
    let cpp = profile.language == Language::Cpp;
    let compiler = if cpp { "/usr/bin/g++" } else { "/usr/bin/gcc" };
    let cwd = Path::new("/build");
    require(
        checked(&args(&[compiler, "-dumpmachine"]), cwd)?.trim() == config.target,
        "compiler target mismatch",
    )?;
    require(
        checked(&args(&[compiler, "-dumpversion"]), cwd)?.trim() == "12",
        "compiler version mismatch",
    )?;
    let mut command = vec![compiler.to_string()];
    command.extend(config.flags.clone());
    command.extend(args(&[
        if cpp { "-std=c++23" } else { "-std=c17" },
        "-c",
        if cpp {
            "/oracle/consumer.cpp"
        } else {
            "/oracle/consumer.c"
        },
        "-o",
        "/build/consumer.o",
    ]));
    checked(&command, cwd)?;
    let mut objects = vec!["/build/consumer.o".into()];
    if !cpp {
        let mut command = vec![compiler.to_string()];
        command.extend(config.flags.clone());
        command.extend(args(&[
            "-std=c17",
            "-c",
            "/snapshot/implementations/quota/quota.c",
            "-o",
            "/build/quota.o",
        ]));
        checked(&command, cwd)?;
        objects.push("/build/quota.o".into());
    }
    let mut command = vec![compiler.to_string()];
    command.extend(config.flags.clone());
    command.extend(objects);
    command.extend(args(&["-o", "/build/consumer"]));
    checked(&command, cwd)?;
    let cases: Vec<String> = read("/oracle/cases.json")?;
    crate::unique(cases.iter().map(String::as_str))?;
    let mut report = Inventory {
        schema_version: 1,
        discovered: cases.clone(),
        selected: cases.clone(),
        ..Default::default()
    };
    for case in cases {
        let output = run(&args(&["/build/consumer", &case]), cwd, &[])?;
        if output.code == Some(0) {
            report.passed.push(case);
        } else {
            report.failed.push(case);
        }
    }
    report.validate()?;
    write_json("/outputs/cases.json", &report)?;
    Ok(i32::from(!report.failed.is_empty()))
}
