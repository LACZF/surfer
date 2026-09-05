use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
};

use camino::{Utf8Path, Utf8PathBuf};

/// Extract dependencies from a VHDL file by scanning for:
/// `use <library>.<package>`, component declarations/instantiations,
/// and direct entity instantiations.
fn extract_vhdl_dependencies(path: &Utf8Path) -> HashSet<String> {
    let content = fs::read_to_string(path).unwrap_or_default().to_lowercase();
    let mut deps = HashSet::new();
    let mut declared_components = HashSet::new();

    for line in content.lines() {
        let line = line.split("--").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        // Capture: use work.pkg_name.all  or  use work.pkg_name
        if line.starts_with("use ") {
            let parts: Vec<&str> = line.trim_end_matches(';').split('.').collect();
            // parts[0] = "use work", parts[1] = package_name
            if parts.len() >= 2 {
                let library = parts[0].trim_start_matches("use").trim();
                let package = parts[1];
                if library == "work" {
                    deps.insert(package.to_string());
                }
            }
            continue;
        }

        // Capture: component foo is
        if let Some(rest) = line.strip_prefix("component ") {
            if !line.starts_with("end component") {
                let component_name = rest
                    .split_whitespace()
                    .next()
                    .map(|name| name.trim_end_matches(';'));
                if let Some(component_name) = component_name
                    && !component_name.is_empty()
                    && component_name != "is"
                {
                    declared_components.insert(component_name.to_string());
                }
            }
            continue;
        }

        // Capture instantiations: label: entity work.foo(...)  OR  label: foo ...
        if let Some((_, rhs)) = line.split_once(':') {
            let rhs = rhs.trim();

            if let Some(rest) = rhs.strip_prefix("entity ") {
                if let Some(designator) = rest.split_whitespace().next() {
                    let designator = designator
                        .trim_end_matches(';')
                        .split('(')
                        .next()
                        .unwrap_or(designator);

                    if let Some((library, name)) = designator.split_once('.') {
                        if library == "work" && !name.is_empty() {
                            deps.insert(name.to_string());
                        }
                    } else if !designator.is_empty() {
                        deps.insert(designator.to_string());
                    }
                }
                continue;
            }

            if let Some(component_name) = rhs.split_whitespace().next() {
                let component_name = component_name.trim_end_matches(';');
                if declared_components.contains(component_name) {
                    deps.insert(component_name.to_string());
                }
            }
        }
    }
    deps
}

/// Extract the "provided name" of a VHDL file - the package, entity, or architecture name.
fn extract_vhdl_provided_name(path: &Utf8Path) -> Option<String> {
    let content = fs::read_to_string(path).unwrap_or_default().to_lowercase();
    for line in content.lines() {
        let line = line.trim();
        for keyword in &["package ", "entity "] {
            if line.starts_with(keyword) && !line.contains("body") {
                let rest = line[keyword.len()..].trim();
                let name = rest.split_whitespace().next()?;
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Topological sort using Kahn's algorithm.
/// Returns sorted file list or an error listing any cycle members.
fn topological_sort(
    files: &[Utf8PathBuf],
    deps: &HashMap<Utf8PathBuf, HashSet<Utf8PathBuf>>, // file -> set of files it depends on
) -> Result<Vec<Utf8PathBuf>, Vec<Utf8PathBuf>> {
    // Build in-degree map and adjacency list
    let mut in_degree: HashMap<Utf8PathBuf, usize> =
        files.iter().cloned().map(|f| (f, 0)).collect();
    let mut adj: HashMap<Utf8PathBuf, Vec<Utf8PathBuf>> =
        files.iter().cloned().map(|f| (f, vec![])).collect();

    for file in files {
        if let Some(file_deps) = deps.get(file) {
            for dep in file_deps {
                // dep must come before file
                if let Some(neighbors) = adj.get_mut(dep) {
                    neighbors.push(file.clone());
                }
                *in_degree.entry(file.clone()).or_insert(0) += 1;
            }
        }
    }

    // Start with nodes that have no dependencies
    let mut queue: VecDeque<Utf8PathBuf> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(f, _)| f.clone())
        .collect();

    let mut sorted = Vec::new();
    while let Some(node) = queue.pop_front() {
        sorted.push(node.clone());
        if let Some(neighbors) = adj.get(&node) {
            for neighbor in neighbors {
                let deg = in_degree
                    .get_mut(neighbor)
                    .expect("neighbor must exist in in_degree");
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(neighbor.clone());
                }
            }
        }
    }

    if sorted.len() == files.len() {
        Ok(sorted)
    } else {
        // Return files still stuck in a cycle
        let sorted_set: HashSet<&Utf8PathBuf> = sorted.iter().collect();
        let cycle_members = files
            .iter()
            .filter(|f| !sorted_set.contains(f))
            .cloned()
            .collect();
        Err(cycle_members)
    }
}

/// Determine a valid compilation order for VHDL files based on their dependencies.
pub(crate) fn vhdl_compile_order(
    files: &[Utf8PathBuf],
) -> Result<Vec<Utf8PathBuf>, Vec<Utf8PathBuf>> {
    // Map: provided_name -> file_path
    let mut name_to_file: HashMap<String, Utf8PathBuf> = HashMap::new();
    for file in files {
        if let Some(name) = extract_vhdl_provided_name(file.as_path()) {
            name_to_file.insert(name, file.clone());
        }
    }

    // Map: file_path -> set of file_paths it depends on
    let mut file_deps: HashMap<Utf8PathBuf, HashSet<Utf8PathBuf>> = HashMap::new();
    for file in files {
        let dep_names = extract_vhdl_dependencies(file.as_path());
        let dep_files: HashSet<Utf8PathBuf> = dep_names
            .iter()
            .filter_map(|name| name_to_file.get(name).cloned())
            .collect();
        file_deps.insert(file.clone(), dep_files);
    }

    topological_sort(files, &file_deps)
}

#[cfg(test)]
mod tests {
    use super::{extract_vhdl_dependencies, extract_vhdl_provided_name, vhdl_compile_order};
    use camino::Utf8PathBuf;
    use std::fs;
    use tempfile::TempDir;

    fn make_temp_dir() -> (TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().expect("failed to create temp test directory");
        let utf8 = Utf8PathBuf::from_path_buf(dir.path().to_path_buf())
            .expect("temp directory path is not valid UTF-8");
        (dir, utf8)
    }

    fn write_vhdl_file(dir: &Utf8PathBuf, name: &str, content: &str) -> Utf8PathBuf {
        let path = dir.join(name);
        fs::write(path.as_std_path(), content).expect("failed to write VHDL test file");
        path
    }

    #[test]
    fn extracts_provided_name_and_dependencies() {
        let (_tempdir, dir) = make_temp_dir();
        let file = write_vhdl_file(
            &dir,
            "top.vhd",
            "
                use work.util_pkg.all;
                entity top is end top;
                architecture rtl of top is
                    component child is
                    end component;
                begin
                    u0: child port map();
                    u1: entity work.peer(rtl) port map();
                end architecture;
            ",
        );

        let provided = extract_vhdl_provided_name(file.as_path());
        let deps = extract_vhdl_dependencies(file.as_path());

        assert_eq!(provided.as_deref(), Some("top"));
        assert!(deps.contains("util_pkg"));
        assert!(deps.contains("child"));
        assert!(deps.contains("peer"));
    }

    #[test]
    fn compile_order_places_dependencies_first() {
        let (_tempdir, dir) = make_temp_dir();
        let pkg = write_vhdl_file(&dir, "util_pkg.vhd", "package util_pkg is end package;");
        let child = write_vhdl_file(&dir, "child.vhd", "entity child is end child;");
        let top = write_vhdl_file(
            &dir,
            "top.vhd",
            "
                use work.util_pkg.all;
                entity top is end top;
                architecture rtl of top is
                begin
                    u1: entity work.child(rtl) port map();
                end architecture;
            ",
        );

        let files = vec![top.clone(), child.clone(), pkg.clone()];
        let order = vhdl_compile_order(&files).expect("expected acyclic compile order");

        let top_idx = order
            .iter()
            .position(|f| f == &top)
            .expect("top missing from compile order");
        let child_idx = order
            .iter()
            .position(|f| f == &child)
            .expect("child missing from compile order");
        let pkg_idx = order
            .iter()
            .position(|f| f == &pkg)
            .expect("package missing from compile order");

        assert!(child_idx < top_idx);
        assert!(pkg_idx < top_idx);
    }

    #[test]
    fn compile_order_reports_cycle_members() {
        let (_tempdir, dir) = make_temp_dir();
        let a = write_vhdl_file(
            &dir,
            "a.vhd",
            "
                entity a is end a;
                architecture rtl of a is
                begin
                    u1: entity work.b(rtl) port map();
                end architecture;
            ",
        );
        let b = write_vhdl_file(
            &dir,
            "b.vhd",
            "
                entity b is end b;
                architecture rtl of b is
                begin
                    u1: entity work.a(rtl) port map();
                end architecture;
            ",
        );

        let files = vec![a.clone(), b.clone()];
        let cycle = vhdl_compile_order(&files).expect_err("expected cycle detection to fail");

        assert_eq!(cycle.len(), 2);
        assert!(cycle.contains(&a));
        assert!(cycle.contains(&b));
    }
}
