// Copyright 2018-2020 the Deno authors. All rights reserved. MIT license.
use crate::flags::Flags;
use log::Level;
use regex::{Regex, RegexBuilder};
use std::env;
use std::fs;
use std::fs::File;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Write;
#[cfg(not(windows))]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use url::Url;

lazy_static! {
    static ref EXEC_NAME_RE: Regex = RegexBuilder::new(
        r"^[a-z][\w-]*$"
    ).case_insensitive(true).build().unwrap();
    // Regular expression to test disk driver letter. eg "C:\\User\username\path\to"
    static ref DRIVE_LETTER_REG: Regex = RegexBuilder::new(
        r"^[c-z]:"
    ).case_insensitive(true).build().unwrap();
}

pub fn is_remote_url(module_url: &str) -> bool {
  let lower = module_url.to_lowercase();
  lower.starts_with("http://") || lower.starts_with("https://")
}

fn validate_name(exec_name: &str) -> Result<(), Error> {
  if EXEC_NAME_RE.is_match(exec_name) {
    Ok(())
  } else {
    Err(Error::new(
      ErrorKind::Other,
      format!("Invalid executable name: {}", exec_name),
    ))
  }
}

#[cfg(windows)]
/// On Windows if user is using Powershell .cmd extension is need to run the
/// installed module.
/// Generate batch script to satisfy that.
fn generate_executable_file(
  file_path: PathBuf,
  args: Vec<String>,
) -> Result<(), Error> {
  let args: Vec<String> = args.iter().map(|c| format!("\"{}\"", c)).collect();
  let template = format!(
    "% generated by deno install %\n@deno.exe {} %*\n",
    args.join(" ")
  );
  let mut file = File::create(&file_path)?;
  file.write_all(template.as_bytes())?;
  Ok(())
}

#[cfg(not(windows))]
fn generate_executable_file(
  file_path: PathBuf,
  args: Vec<String>,
) -> Result<(), Error> {
  let args: Vec<String> = args.iter().map(|c| format!("\"{}\"", c)).collect();
  let template = format!(
    r#"#!/bin/sh
# generated by deno install
deno {} "$@"
"#,
    args.join(" "),
  );
  let mut file = File::create(&file_path)?;
  file.write_all(template.as_bytes())?;
  let _metadata = fs::metadata(&file_path)?;
  let mut permissions = _metadata.permissions();
  permissions.set_mode(0o755);
  fs::set_permissions(&file_path, permissions)?;
  Ok(())
}

fn generate_config_file(
  file_path: PathBuf,
  config_file_name: String,
) -> Result<(), Error> {
  let config_file_copy_path = get_config_file_path(&file_path);
  let cwd = std::env::current_dir().unwrap();
  let config_file_path = cwd.join(config_file_name);
  fs::copy(config_file_path, config_file_copy_path)?;
  Ok(())
}

fn get_installer_root() -> Result<PathBuf, Error> {
  if let Ok(env_dir) = env::var("DENO_INSTALL_ROOT") {
    if !env_dir.is_empty() {
      return PathBuf::from(env_dir).canonicalize();
    }
  }
  // Note: on Windows, the $HOME environment variable may be set by users or by
  // third party software, but it is non-standard and should not be relied upon.
  let home_env_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
  let mut home_path =
    env::var_os(home_env_var)
      .map(PathBuf::from)
      .ok_or_else(|| {
        Error::new(
          ErrorKind::NotFound,
          format!("${} is not defined", home_env_var),
        )
      })?;
  home_path.push(".deno");
  Ok(home_path)
}

fn infer_name_from_url(url: &Url) -> Option<String> {
  let path = PathBuf::from(url.path());
  let stem = match path.file_stem() {
    Some(stem) => stem.to_string_lossy().to_string(),
    None => return None,
  };
  if let Some(parent_path) = path.parent() {
    if stem == "main" || stem == "mod" || stem == "index" || stem == "cli" {
      if let Some(parent_name) = parent_path.file_name() {
        return Some(parent_name.to_string_lossy().to_string());
      }
    }
  }
  Some(stem)
}

pub fn install(
  flags: Flags,
  module_url: &str,
  args: Vec<String>,
  name: Option<String>,
  root: Option<PathBuf>,
  force: bool,
) -> Result<(), Error> {
  let root = if let Some(root) = root {
    root.canonicalize()?
  } else {
    get_installer_root()?
  };
  let installation_dir = root.join("bin");

  // ensure directory exists
  if let Ok(metadata) = fs::metadata(&installation_dir) {
    if !metadata.is_dir() {
      return Err(Error::new(
        ErrorKind::Other,
        "Installation path is not a directory",
      ));
    }
  } else {
    fs::create_dir_all(&installation_dir)?;
  };

  // Check if module_url is remote
  let module_url = if is_remote_url(module_url) {
    Url::parse(module_url).expect("Should be valid url")
  } else {
    let module_path = PathBuf::from(module_url);
    let module_path = if module_path.is_absolute() {
      module_path
    } else {
      let cwd = env::current_dir().unwrap();
      cwd.join(module_path)
    };
    Url::from_file_path(module_path).expect("Path should be absolute")
  };

  let name = name.or_else(|| infer_name_from_url(&module_url));

  let name = match name {
    Some(name) => name,
    None => return Err(Error::new(
      ErrorKind::Other,
      "An executable name was not provided. One could not be inferred from the URL. Aborting.",
    )),
  };

  validate_name(name.as_str())?;
  let mut file_path = installation_dir.join(&name);

  if cfg!(windows) {
    file_path = file_path.with_extension("cmd");
  }

  if file_path.exists() && !force {
    return Err(Error::new(
      ErrorKind::Other,
      "Existing installation found. Aborting (Use -f to overwrite).",
    ));
  };

  let mut executable_args = vec!["run".to_string()];
  executable_args.extend_from_slice(&flags.to_permission_args());
  if let Some(ca_file) = flags.ca_file {
    executable_args.push("--cert".to_string());
    executable_args.push(ca_file)
  }
  if let Some(log_level) = flags.log_level {
    if log_level == Level::Error {
      executable_args.push("--quiet".to_string());
    } else {
      executable_args.push("--log-level".to_string());
      let log_level = match log_level {
        Level::Debug => "debug",
        Level::Info => "info",
        _ => {
          return Err(Error::new(
            ErrorKind::Other,
            format!("invalid log level {}", log_level),
          ))
        }
      };
      executable_args.push(log_level.to_string());
    }
  }

  if flags.no_check {
    executable_args.push("--no-check".to_string());
  }

  if flags.unstable {
    executable_args.push("--unstable".to_string());
  }

  if flags.config_path.is_some() {
    let config_file_path = get_config_file_path(&file_path);
    let config_file_path_option = config_file_path.to_str();
    if let Some(config_file_path_string) = config_file_path_option {
      executable_args.push("--config".to_string());
      executable_args.push(config_file_path_string.to_string());
    }
  }

  executable_args.push(module_url.to_string());
  executable_args.extend_from_slice(&args);

  generate_executable_file(file_path.to_owned(), executable_args)?;
  if let Some(config_path) = flags.config_path {
    generate_config_file(file_path.to_owned(), config_path)?;
  }

  println!("✅ Successfully installed {}", name);
  println!("{}", file_path.to_string_lossy());
  let installation_dir_str = installation_dir.to_string_lossy();

  if !is_in_path(&installation_dir) {
    println!("ℹ️  Add {} to PATH", installation_dir_str);
    if cfg!(windows) {
      println!("    set PATH=%PATH%;{}", installation_dir_str);
    } else {
      println!("    export PATH=\"{}:$PATH\"", installation_dir_str);
    }
  }

  Ok(())
}

fn is_in_path(dir: &PathBuf) -> bool {
  if let Some(paths) = env::var_os("PATH") {
    for p in env::split_paths(&paths) {
      if *dir == p {
        return true;
      }
    }
  }
  false
}

fn get_config_file_path(file_path: &PathBuf) -> PathBuf {
  let mut config_file_copy_path = PathBuf::from(file_path);
  config_file_copy_path.set_extension("tsconfig.json");
  config_file_copy_path
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;
  use tempfile::TempDir;

  lazy_static! {
    pub static ref ENV_LOCK: Mutex<()> = Mutex::new(());
  }

  #[test]
  fn test_is_remote_url() {
    assert!(is_remote_url("https://deno.land/std/http/file_server.ts"));
    assert!(is_remote_url("http://deno.land/std/http/file_server.ts"));
    assert!(is_remote_url("HTTP://deno.land/std/http/file_server.ts"));
    assert!(is_remote_url("HTTp://deno.land/std/http/file_server.ts"));
    assert!(!is_remote_url("file:///dev/deno_std/http/file_server.ts"));
    assert!(!is_remote_url("./dev/deno_std/http/file_server.ts"));
  }

  #[test]
  fn install_infer_name_from_url() {
    assert_eq!(
      infer_name_from_url(
        &Url::parse("https://example.com/abc/server.ts").unwrap()
      ),
      Some("server".to_string())
    );
    assert_eq!(
      infer_name_from_url(
        &Url::parse("https://example.com/abc/main.ts").unwrap()
      ),
      Some("abc".to_string())
    );
    assert_eq!(
      infer_name_from_url(
        &Url::parse("https://example.com/abc/mod.ts").unwrap()
      ),
      Some("abc".to_string())
    );
    assert_eq!(
      infer_name_from_url(
        &Url::parse("https://example.com/abc/index.ts").unwrap()
      ),
      Some("abc".to_string())
    );
    assert_eq!(
      infer_name_from_url(
        &Url::parse("https://example.com/abc/cli.ts").unwrap()
      ),
      Some("abc".to_string())
    );
    assert_eq!(
      infer_name_from_url(&Url::parse("https://example.com/main.ts").unwrap()),
      Some("main".to_string())
    );
    assert_eq!(
      infer_name_from_url(&Url::parse("https://example.com").unwrap()),
      None
    );
    assert_eq!(
      infer_name_from_url(&Url::parse("file:///abc/server.ts").unwrap()),
      Some("server".to_string())
    );
    assert_eq!(
      infer_name_from_url(&Url::parse("file:///abc/main.ts").unwrap()),
      Some("abc".to_string())
    );
    assert_eq!(
      infer_name_from_url(&Url::parse("file:///main.ts").unwrap()),
      Some("main".to_string())
    );
    assert_eq!(infer_name_from_url(&Url::parse("file:///").unwrap()), None);
  }

  #[test]
  fn install_basic() {
    let _guard = ENV_LOCK.lock().ok();
    let temp_dir = TempDir::new().expect("tempdir fail");
    let temp_dir_str = temp_dir.path().to_string_lossy().to_string();
    // NOTE: this test overrides environmental variables
    // don't add other tests in this file that mess with "HOME" and "USEPROFILE"
    // otherwise transient failures are possible because tests are run in parallel.
    // It means that other test can override env vars when this test is running.
    let original_home = env::var_os("HOME");
    let original_user_profile = env::var_os("HOME");
    let original_install_root = env::var_os("DENO_INSTALL_ROOT");
    env::set_var("HOME", &temp_dir_str);
    env::set_var("USERPROFILE", &temp_dir_str);
    env::set_var("DENO_INSTALL_ROOT", "");

    install(
      Flags::default(),
      "http://localhost:4545/cli/tests/echo_server.ts",
      vec![],
      Some("echo_test".to_string()),
      None,
      false,
    )
    .expect("Install failed");

    let mut file_path = temp_dir.path().join(".deno/bin/echo_test");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }

    assert!(file_path.exists());

    let content = fs::read_to_string(file_path).unwrap();
    // It's annoying when shell scripts don't have NL at the end.
    assert_eq!(content.chars().last().unwrap(), '\n');

    assert!(content
      .contains(r#""run" "http://localhost:4545/cli/tests/echo_server.ts""#));
    if let Some(home) = original_home {
      env::set_var("HOME", home);
    }
    if let Some(user_profile) = original_user_profile {
      env::set_var("USERPROFILE", user_profile);
    }
    if let Some(install_root) = original_install_root {
      env::set_var("DENO_INSTALL_ROOT", install_root);
    }
  }

  #[test]
  fn install_unstable() {
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();

    install(
      Flags {
        unstable: true,
        ..Flags::default()
      },
      "http://localhost:4545/cli/tests/echo_server.ts",
      vec![],
      Some("echo_test".to_string()),
      Some(temp_dir.path().to_path_buf()),
      false,
    )
    .expect("Install failed");

    let mut file_path = bin_dir.join("echo_test");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }

    assert!(file_path.exists());

    let content = fs::read_to_string(file_path).unwrap();
    println!("this is the file path {:?}", content);
    assert!(content.contains(
      r#""run" "--unstable" "http://localhost:4545/cli/tests/echo_server.ts"#
    ));
  }

  #[test]
  fn install_inferred_name() {
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();

    install(
      Flags::default(),
      "http://localhost:4545/cli/tests/echo_server.ts",
      vec![],
      None,
      Some(temp_dir.path().to_path_buf()),
      false,
    )
    .expect("Install failed");

    let mut file_path = bin_dir.join("echo_server");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }

    assert!(file_path.exists());
    let content = fs::read_to_string(file_path).unwrap();
    assert!(content
      .contains(r#""run" "http://localhost:4545/cli/tests/echo_server.ts""#));
  }

  #[test]
  fn install_inferred_name_from_parent() {
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();

    install(
      Flags::default(),
      "http://localhost:4545/cli/tests/subdir/main.ts",
      vec![],
      None,
      Some(temp_dir.path().to_path_buf()),
      false,
    )
    .expect("Install failed");

    let mut file_path = bin_dir.join("subdir");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }

    assert!(file_path.exists());
    let content = fs::read_to_string(file_path).unwrap();
    assert!(content
      .contains(r#""run" "http://localhost:4545/cli/tests/subdir/main.ts""#));
  }

  #[test]
  fn install_custom_dir_option() {
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();

    install(
      Flags::default(),
      "http://localhost:4545/cli/tests/echo_server.ts",
      vec![],
      Some("echo_test".to_string()),
      Some(temp_dir.path().to_path_buf()),
      false,
    )
    .expect("Install failed");

    let mut file_path = bin_dir.join("echo_test");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }

    assert!(file_path.exists());
    let content = fs::read_to_string(file_path).unwrap();
    assert!(content
      .contains(r#""run" "http://localhost:4545/cli/tests/echo_server.ts""#));
  }

  #[test]
  fn install_custom_dir_env_var() {
    let _guard = ENV_LOCK.lock().ok();
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();
    let original_install_root = env::var_os("DENO_INSTALL_ROOT");
    env::set_var("DENO_INSTALL_ROOT", temp_dir.path().to_path_buf());

    install(
      Flags::default(),
      "http://localhost:4545/cli/tests/echo_server.ts",
      vec![],
      Some("echo_test".to_string()),
      None,
      false,
    )
    .expect("Install failed");

    let mut file_path = bin_dir.join("echo_test");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }

    assert!(file_path.exists());
    let content = fs::read_to_string(file_path).unwrap();
    assert!(content
      .contains(r#""run" "http://localhost:4545/cli/tests/echo_server.ts""#));
    if let Some(install_root) = original_install_root {
      env::set_var("DENO_INSTALL_ROOT", install_root);
    }
  }

  #[test]
  fn install_with_flags() {
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();

    install(
      Flags {
        allow_net: true,
        allow_read: true,
        no_check: true,
        log_level: Some(Level::Error),
        ..Flags::default()
      },
      "http://localhost:4545/cli/tests/echo_server.ts",
      vec!["--foobar".to_string()],
      Some("echo_test".to_string()),
      Some(temp_dir.path().to_path_buf()),
      false,
    )
    .expect("Install failed");

    let mut file_path = bin_dir.join("echo_test");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }

    assert!(file_path.exists());
    let content = fs::read_to_string(file_path).unwrap();
    assert!(content.contains(r#""run" "--allow-read" "--allow-net" "--quiet" "--no-check" "http://localhost:4545/cli/tests/echo_server.ts" "--foobar""#));
  }

  #[test]
  fn install_local_module() {
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();
    let local_module = env::current_dir().unwrap().join("echo_server.ts");
    let local_module_url = Url::from_file_path(&local_module).unwrap();
    let local_module_str = local_module.to_string_lossy();

    install(
      Flags::default(),
      &local_module_str,
      vec![],
      Some("echo_test".to_string()),
      Some(temp_dir.path().to_path_buf()),
      false,
    )
    .expect("Install failed");

    let mut file_path = bin_dir.join("echo_test");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }

    assert!(file_path.exists());
    let content = fs::read_to_string(file_path).unwrap();
    assert!(content.contains(&local_module_url.to_string()));
  }

  #[test]
  fn install_force() {
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();

    install(
      Flags::default(),
      "http://localhost:4545/cli/tests/echo_server.ts",
      vec![],
      Some("echo_test".to_string()),
      Some(temp_dir.path().to_path_buf()),
      false,
    )
    .expect("Install failed");

    let mut file_path = bin_dir.join("echo_test");
    if cfg!(windows) {
      file_path = file_path.with_extension("cmd");
    }
    assert!(file_path.exists());

    // No force. Install failed.
    let no_force_result = install(
      Flags::default(),
      "http://localhost:4545/cli/tests/cat.ts", // using a different URL
      vec![],
      Some("echo_test".to_string()),
      Some(temp_dir.path().to_path_buf()),
      false,
    );
    assert!(no_force_result.is_err());
    assert!(no_force_result
      .unwrap_err()
      .to_string()
      .contains("Existing installation found"));
    // Assert not modified
    let file_content = fs::read_to_string(&file_path).unwrap();
    assert!(file_content.contains("echo_server.ts"));

    // Force. Install success.
    let force_result = install(
      Flags::default(),
      "http://localhost:4545/cli/tests/cat.ts", // using a different URL
      vec![],
      Some("echo_test".to_string()),
      Some(temp_dir.path().to_path_buf()),
      true,
    );
    assert!(force_result.is_ok());
    // Assert modified
    let file_content_2 = fs::read_to_string(&file_path).unwrap();
    assert!(file_content_2.contains("cat.ts"));
  }

  #[test]
  fn install_with_config() {
    let temp_dir = TempDir::new().expect("tempdir fail");
    let bin_dir = temp_dir.path().join("bin");
    let config_file_path = temp_dir.path().join("test_tsconfig.json");
    let config = "{}";
    let mut config_file = File::create(&config_file_path).unwrap();
    let result = config_file.write_all(config.as_bytes());
    assert!(result.is_ok());

    let result = install(
      Flags {
        config_path: Some(config_file_path.to_string_lossy().to_string()),
        ..Flags::default()
      },
      "http://localhost:4545/cli/tests/cat.ts",
      vec![],
      Some("echo_test".to_string()),
      Some(temp_dir.path().to_path_buf()),
      true,
    );
    eprintln!("result {:?}", result);
    assert!(result.is_ok());

    let config_file_name = "echo_test.tsconfig.json";

    let file_path = bin_dir.join(config_file_name.to_string());
    assert!(file_path.exists());
    let content = fs::read_to_string(file_path).unwrap();
    assert!(content == "{}");
  }
}
