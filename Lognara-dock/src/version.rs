pub fn release_version() -> String {
    resolve_release_version(
        env!("CARGO_PKG_VERSION"),
        option_env!("LOGNARA_RELEASE_VERSION"),
        option_env!("LOGNARA_GIT_COMMIT"),
    )
}

fn resolve_release_version(
    package_version: &str,
    release_version: Option<&str>,
    git_commit: Option<&str>,
) -> String {
    if let Some(version) = release_version.filter(|version| !version.trim().is_empty()) {
        return version.trim().to_owned();
    }
    match git_commit
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
    {
        Some(commit) => format!(
            "{package_version}+{}",
            commit.chars().take(7).collect::<String>()
        ),
        None => package_version.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_release_version;

    #[test]
    fn uses_published_version_and_falls_back_to_package_and_short_commit() {
        assert_eq!(
            resolve_release_version("0.1.0", Some("dev-abcdef012345"), Some("1234567890")),
            "dev-abcdef012345"
        );
        assert_eq!(
            resolve_release_version("0.1.0", Some(""), Some("1234567890")),
            "0.1.0+1234567"
        );
        assert_eq!(resolve_release_version("0.1.0", None, None), "0.1.0");
    }
}
