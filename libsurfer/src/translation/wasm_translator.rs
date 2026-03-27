use std::ffi::OsString;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use extism::{Manifest, PTR, Plugin, PluginBuilder, Wasm, host_fn};
use extism_convert;
use extism_manifest::MemoryOptions;
use eyre::{WrapErr as _, anyhow};
use surfer_translation_types::plugin_types::TranslateParams;
use surfer_translation_types::{
    TranslationPreference, TranslationResult, Translator, VariableInfo, VariableMeta,
    VariableNameInfo, VariableValue,
};
use tracing::{error, info, warn};

use crate::config::{LOCAL_DIR, PROJECT_DIR};
use crate::message::Message;
use crate::wave_container::{ScopeId, VarId};

pub static TRANSLATOR_DIR: &str = "translators";

pub fn discover_wasm_translators() -> Vec<Message> {
    let search_dirs = [
        std::env::current_dir()
            .ok()
            .map(|dir| dir.join(LOCAL_DIR).join(TRANSLATOR_DIR)),
        PROJECT_DIR
            .as_ref()
            .map(|dirs| dirs.data_dir().join(TRANSLATOR_DIR)),
    ]
    .into_iter()
    .flatten();

    let plugin_files = search_dirs
        .into_iter()
        .flat_map(|dir| {
            info!("Looking for translators in {}", dir.display());
            if !dir.exists() {
                return vec![];
            }
            read_dir(&dir)
                .map(|readdir| {
                    readdir
                        .filter_map(|entry| match entry {
                            Ok(entry) => {
                                let path = entry.path();
                                if path.extension() == Some(&OsString::from("wasm")) {
                                    info!("Found {}", path.display());
                                    Some(path)
                                } else {
                                    None
                                }
                            }
                            Err(e) => {
                                warn!("Failed to read entry in {:?}. {e}", dir.to_string_lossy());
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .map_err(|e| {
                    warn!(
                        "Failed to read dir entries in {}. {e}",
                        dir.to_string_lossy()
                    );
                })
                .unwrap_or_else(|()| vec![])
        })
        .filter_map(|file| {
            file.clone()
                .try_into()
                .map_err(|_| {
                    format!(
                        "{} is not a valid UTF8 path, ignoring this translator",
                        file.to_string_lossy()
                    )
                })
                .ok()
        });

    plugin_files.map(Message::LoadWasmTranslator).collect()
}

pub struct PluginTranslator {
    plugin: Arc<Mutex<Plugin>>,
    file: PathBuf,
    max_memory_mib: u64,
}

impl PluginTranslator {
    pub fn new(file: PathBuf, max_memory_mib: u64) -> eyre::Result<Self> {
        let data = std::fs::read(&file)
            .with_context(|| format!("Failed to read {}", file.to_string_lossy()))?;

        let manifest = Manifest::new([Wasm::data(data)])
            .with_memory_options(
                MemoryOptions::new().with_max_var_bytes(max_memory_mib * 1024 * 1024),
            )
            .with_allowed_path("ro:/".to_string(), "/")
            .disallow_all_hosts();
        let mut plugin = PluginBuilder::new(manifest)
            .with_wasi(true)
            .with_debug_info()
            .with_function(
                "read_file",
                [PTR],
                [PTR],
                extism::UserData::new(()),
                read_file,
            )
            .with_function(
                "file_exists",
                [PTR],
                [PTR],
                extism::UserData::new(()),
                file_exists,
            )
            .with_function(
                "translators_config_dir",
                [],
                [PTR],
                extism::UserData::new(()),
                translators_config_dir,
            )
            .build()
            .map_err(|e| anyhow!("Failed to load plugin from {} {e}", file.to_string_lossy()))?;

        if plugin.function_exists("new") {
            plugin
                .call::<_, ()>("new", ())
                .map_err(|e| Self::enrich_error(e, "new", &file, max_memory_mib))?;
        }

        Ok(Self {
            plugin: Arc::new(Mutex::new(plugin)),
            file,
            max_memory_mib,
        })
    }

    fn enrich_error(
        e: extism::Error,
        func: &str,
        file: &Path,
        max_memory_mib: u64,
    ) -> eyre::Report {
        let mut msg = format!(
            "Failed to call `{func}` on plugin from {}. {e}",
            file.to_string_lossy()
        );
        if e.to_string().contains("oom") {
            msg.push_str(&format!(
                "\nPlugin ran out of memory ({max_memory_mib} MiB). \
                Increase `plugin.max_memory_mib` in your surfer config."
            ));
        }
        anyhow!("{msg}")
    }
}

impl Translator<VarId, ScopeId, Message> for PluginTranslator {
    fn name(&self) -> String {
        self.plugin
            .lock()
            .unwrap()
            .call::<_, &str>("name", ())
            .map_err(|e| {
                error!(
                    "{:#}",
                    Self::enrich_error(e, "name", &self.file, self.max_memory_mib)
                );
            })
            .map(ToString::to_string)
            .unwrap_or_default()
    }

    fn set_wave_source(&self, wave_source: Option<surfer_translation_types::WaveSource>) {
        let mut plugin = self.plugin.lock().unwrap();
        if plugin.function_exists("set_wave_source") {
            plugin
                .call::<_, ()>("set_wave_source", extism_convert::Json(wave_source))
                .map_err(|e| {
                    error!(
                        "{:#}",
                        Self::enrich_error(e, "set_wave_source", &self.file, self.max_memory_mib)
                    );
                })
                .ok();
        }
    }

    fn translate(
        &self,
        variable: &VariableMeta<VarId, ScopeId>,
        value: &VariableValue,
    ) -> eyre::Result<TranslationResult> {
        let result = self
            .plugin
            .lock()
            .unwrap()
            .call(
                "translate",
                TranslateParams {
                    variable: variable.clone().map_ids(|_| (), |_| ()),
                    value: value.clone(),
                },
            )
            .map_err(|e| Self::enrich_error(e, "translate", &self.file, self.max_memory_mib))?;
        Ok(result)
    }

    fn variable_info(&self, variable: &VariableMeta<VarId, ScopeId>) -> eyre::Result<VariableInfo> {
        let result = self
            .plugin
            .lock()
            .unwrap()
            .call("variable_info", variable.clone().map_ids(|_| (), |_| ()))
            .map_err(|e| Self::enrich_error(e, "variable_info", &self.file, self.max_memory_mib))?;
        Ok(result)
    }

    fn translates(
        &self,
        variable: &VariableMeta<VarId, ScopeId>,
    ) -> eyre::Result<TranslationPreference> {
        self.plugin
            .lock()
            .unwrap()
            .call("translates", variable.clone().map_ids(|_| (), |_| ()))
            .map_err(|e| Self::enrich_error(e, "translates", &self.file, self.max_memory_mib))
    }

    fn reload(&self, _sender: std::sync::mpsc::Sender<Message>) {
        let mut plugin = self.plugin.lock().unwrap();
        if plugin.function_exists("reload")
            && let Err(e) = plugin.call::<_, ()>("reload", ())
        {
            error!(
                "{:#}",
                Self::enrich_error(e, "reload", &self.file, self.max_memory_mib)
            );
        }
    }

    fn variable_name_info(
        &self,
        variable: &VariableMeta<VarId, ScopeId>,
    ) -> Option<VariableNameInfo> {
        let mut plugin = self.plugin.lock().unwrap();
        if plugin.function_exists("variable_name_info") {
            match plugin.call(
                "variable_name_info",
                variable.clone().map_ids(|_| (), |_| ()),
            ) {
                Ok(result) => result,
                Err(e) => {
                    error!(
                        "{:#}",
                        Self::enrich_error(
                            e,
                            "variable_name_info",
                            &self.file,
                            self.max_memory_mib
                        )
                    );
                    None
                }
            }
        } else {
            None
        }
    }
}

host_fn!(current_dir() -> String {
    std::env::current_dir()
        .with_context(|| "Failed to get current dir".to_string())
        .and_then(|dir| {
            dir.to_str().ok_or_else(|| {
                anyhow!("{} is not valid utf8", dir.to_string_lossy())
            }).map(ToString::to_string)
        })
        .map_err(|e| extism::Error::msg(format!("{e:#}")))
});

host_fn!(translators_config_dir() -> extism_convert::Json(Option<String>) {
    // Check local .surfer/translators/ first, then fall back to global config dir
    let local = std::env::current_dir()
        .ok()
        .map(|dir| dir.join(LOCAL_DIR).join(TRANSLATOR_DIR))
        .filter(|dir| dir.exists());

    let global = PROJECT_DIR.as_ref()
        .map(|dirs| dirs.config_dir().join("translators"));

    Ok(extism_convert::Json(local.or(global)
        .and_then(|dir| {
            dir.to_str().ok_or_else(|| {
                anyhow!("{} is not valid utf8", dir.to_string_lossy())
            }).map(std::string::ToString::to_string).ok()
        })))
});

host_fn!(read_file(filename: String) -> Vec<u8> {
    std::fs::read(Utf8PathBuf::from(&filename))
        .with_context(|| format!("Failed to read {filename}"))
        .map_err(|e| extism::Error::msg(format!("{e:#}")))
});

host_fn!(file_exists(filename: String) -> bool {
    Ok(Utf8PathBuf::from(&filename).exists())
});

#[cfg(all(test, feature = "wasm_plugins"))]
mod tests {
    use std::collections::HashMap;

    use crate::wave_container::{ScopeId, VarId};
    use project_root::get_project_root;
    use surfer_translation_types::{
        Translator, ValueKind, ValueRepr, VariableEncoding, VariableMeta, VariableValue, WaveSource,
    };

    use super::PluginTranslator;

    fn test_variable() -> VariableMeta<VarId, ScopeId> {
        VariableMeta {
            var: crate::wave_container::VariableRef {
                path: crate::wave_container::ScopeRef {
                    strs: vec![
                        "testbench".to_string(),
                        "top".to_string(),
                        "uut".to_string(),
                    ],
                    id: ScopeId::default(),
                },
                name: "pcpi_insn".to_string(),
                id: VarId::default(),
                index: None,
            },
            num_bits: Some(32),
            variable_type: None,
            variable_type_name: None,
            index: None,
            direction: None,
            enum_map: HashMap::new(),
            encoding: VariableEncoding::BitVector,
        }
    }

    #[test]
    fn example_plugin_can_read_wave_source_via_wasi() {
        let root = get_project_root().unwrap();
        let translator =
            PluginTranslator::new(root.join("examples/wasm_example_translator.wasm"), 10).unwrap();

        translator.set_wave_source(Some(WaveSource::File(
            root.join("examples/picorv32.vcd")
                .to_string_lossy()
                .into_owned(),
        )));

        let result = translator
            .translate(&test_variable(), &VariableValue::String("1010".to_string()))
            .unwrap();

        assert_eq!(result.kind, ValueKind::Normal);
        assert_eq!(result.subfields.len(), 1);
        assert_eq!(result.subfields[0].name, "[0]");
        assert!(result.subfields[0].result.subfields.is_empty());
        assert_eq!(result.subfields[0].result.kind, ValueKind::Normal);
        match &result.subfields[0].result.val {
            ValueRepr::Bits(4, bits) => assert_eq!(bits, "1010"),
            _ => panic!("unexpected translation result"),
        }
    }
}
