use std::io::{self, Write};

/// Launch this exact executable, even after a rebuild replaces its directory entry.
/// Linux's proc link still refers to the running inode; `current_exe()` instead
/// returns a filename ending in ` (deleted)`, which cannot be executed.
pub fn self_executable() -> io::Result<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let executable = std::path::PathBuf::from("/proc/self/exe");
        std::fs::metadata(&executable)?;
        Ok(executable)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::current_exe()
    }
}

pub fn read_line() -> Result<String, String> {
    let mut answer = String::new();
    match io::stdin().read_line(&mut answer) {
        Ok(_) => Ok(answer),
        Err(e) => Err(format!("{:?}", e)),
    }
}

pub fn confirmation(prompt: &str) -> bool {
    println!("{prompt}");
    print!("Enter y|n\n> ");
    io::stdout().flush().unwrap();

    while let Ok(input) = read_line() {
        // End of input: nothing more will arrive, so treat it as no.
        if input.is_empty() {
            return false;
        }
        let input = input.trim();
        if input == "y" {
            return true;
        }

        if input == "n" {
            return false;
        }

        println!("Unknown input.");
        print!("Enter y|n\n> ");
        io::stdout().flush().unwrap();
    }

    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn spawns_after_executable_is_replaced() {
        const CHILD: &str = "FORGEFLOW_TEST_UNLINKED_EXE";
        if let Some(path) = std::env::var_os(CHILD) {
            std::fs::remove_file(path).unwrap();
            assert!(std::env::current_exe().unwrap().to_string_lossy().ends_with(" (deleted)"));
            assert!(Command::new(self_executable().unwrap())
                .arg("--list").stdout(Stdio::null()).status().unwrap().success());
            return;
        }
        let copy = std::env::temp_dir().join(format!("forgeflow-executable-test-{}", uuid::Uuid::now_v7()));
        let original = std::env::current_exe().unwrap();
        if std::fs::hard_link(&original, &copy).is_err() {
            std::fs::copy(&original, &copy).unwrap();
        }
        let result = Command::new(&copy)
            .args(["--exact", "util::tests::spawns_after_executable_is_replaced"])
            .env(CHILD, &copy).output();
        let _ = std::fs::remove_file(&copy);
        let output = result.unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }
}
