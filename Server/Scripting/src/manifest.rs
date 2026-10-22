//! `ivmp.lua`, the resource manifest: a Lua file evaluated in a sandbox that only knows the manifest
//! functions (`server_script`, `client_scripts`, `files`, `ui_page`, ...). A FiveM resource's
//! `fxmanifest.lua` or `__resource.lua` is read the same way when there is no `ivmp.lua`; any other
//! entry (`chat_theme 'x'`) is kept as metadata for `GetResourceMetadata`.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use mlua::{Lua, StdLib, Value, Variadic};

pub const FILE: &str = "ivmp.lua";
/// Manifest file names, in the order they are looked for.
pub const FILES: [&str; 3] = [FILE, "fxmanifest.lua", "__resource.lua"];

/// The manifest file of the resource in `dir`, if it has one.
pub fn find(dir: &Path) -> Option<&'static str> {
    FILES.into_iter().find(|file| dir.join(file).is_file())
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Manifest {
    pub author: String,
    pub description: String,
    pub version: String,
    pub server_scripts: Vec<String>,
    pub client_scripts: Vec<String>,
    pub files: Vec<String>,
    pub ui_page: Option<String>,
    pub dependencies: Vec<String>,
    /// Every other entry, in order: (key, value) (one per string of `key 'a'` / `key { 'a', 'b' }`).
    pub metadata: Vec<(String, String)>,
}

impl Manifest {
    /// The values of a metadata key.
    pub fn metadata_values(&self, key: &str) -> Vec<&str> {
        self.metadata.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect()
    }
}

/// Strings and tables of strings, flattened.
fn strings(values: &Variadic<Value>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values.iter() {
        match value {
            Value::String(s) => out.push(s.to_string_lossy().to_string()),
            Value::Table(t) => {
                for item in t.sequence_values::<String>().flatten() {
                    out.push(item);
                }
            }
            _ => {}
        }
    }
    out
}

pub fn parse(source: &str) -> Result<Manifest, String> {
    let manifest = Rc::new(RefCell::new(Manifest::default()));
    let lua = Lua::new_with(StdLib::NONE, mlua::LuaOptions::default()).map_err(|e| e.to_string())?;
    let globals = lua.globals();
    macro_rules! list {
        ($($name:literal => $field:ident),* $(,)?) => {$(
            let target = manifest.clone();
            let function = lua
                .create_function(move |_, args: Variadic<Value>| {
                    target.borrow_mut().$field.extend(strings(&args));
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            globals.set($name, function).map_err(|e| e.to_string())?;
        )*};
    }
    list!(
        "server_script" => server_scripts, "server_scripts" => server_scripts,
        "client_script" => client_scripts, "client_scripts" => client_scripts,
        "file" => files, "files" => files,
        "dependency" => dependencies, "dependencies" => dependencies,
        // Both sides run shared scripts.
        "shared_script" => server_scripts, "shared_scripts" => server_scripts,
    );
    for name in ["shared_script", "shared_scripts"] {
        let target = manifest.clone();
        let both: mlua::Function = globals.get(name).map_err(|e| e.to_string())?;
        let function = lua
            .create_function(move |_, args: Variadic<Value>| {
                both.call::<()>(args.clone())?;
                target.borrow_mut().client_scripts.extend(strings(&args));
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        globals.set(name, function).map_err(|e| e.to_string())?;
    }
    macro_rules! text {
        ($($name:literal => $field:ident),* $(,)?) => {$(
            let target = manifest.clone();
            let function = lua
                .create_function(move |_, text: String| {
                    target.borrow_mut().$field = text.into();
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            globals.set($name, function).map_err(|e| e.to_string())?;
        )*};
    }
    text!("author" => author, "description" => description, "version" => version);
    let target = manifest.clone();
    let ui = lua
        .create_function(move |_, page: String| {
            target.borrow_mut().ui_page = Some(page);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    globals.set("ui_page", ui).map_err(|e| e.to_string())?;
    // Accepted and ignored, like FiveM's `fx_version 'cerulean'` / `game 'gta4'`.
    for name in ["ivmp_version", "game", "fx_version", "lua54"] {
        globals
            .set(name, lua.create_function(|_, _: Variadic<Value>| Ok(())).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    // Any other entry is metadata. `key 'value'` and `key { ... }` call the function this returns;
    // it returns itself, so a chained `key 'a' { b = 1 }` works too.
    let target = manifest.clone();
    let metadata = lua
        .create_function(move |lua, (_, key): (Value, String)| {
            let target = target.clone();
            let record = lua.create_function(move |_, (this, args): (Value, Variadic<Value>)| {
                let mut manifest = target.borrow_mut();
                for value in strings(&args) {
                    manifest.metadata.push((key.clone(), value));
                }
                Ok(this)
            })?;
            let entry = lua.create_table()?;
            let meta = lua.create_table()?;
            meta.set("__call", record)?;
            entry.set_metatable(Some(meta))?;
            Ok(entry)
        })
        .map_err(|e| e.to_string())?;
    let meta = lua.create_table().map_err(|e| e.to_string())?;
    meta.set("__index", metadata).map_err(|e| e.to_string())?;
    globals.set_metatable(Some(meta)).map_err(|e| e.to_string())?;
    lua.load(source).set_name(FILE).exec().map_err(|e| e.to_string())?;
    drop(globals);
    let result = manifest.borrow().clone();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_manifest() {
        let manifest = parse(
            r#"
            ivmp_version 'beta'
            author 'Drazox'
            description 'Freeroam'
            version '1.2.3'
            server_scripts { 'server/a.lua', 'server/b.lua' }
            server_script 'c.lua'
            client_scripts { 'client.lua' }
            files { 'ui/index.html', 'ui/app.js' }
            ui_page 'ui/index.html'
            dependencies { 'chat' }
            "#,
        )
        .unwrap();
        assert_eq!(manifest.author, "Drazox");
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(manifest.server_scripts, vec!["server/a.lua", "server/b.lua", "c.lua"]);
        assert_eq!(manifest.client_scripts, vec!["client.lua"]);
        assert_eq!(manifest.ui_page.as_deref(), Some("ui/index.html"));
        assert_eq!(manifest.dependencies, vec!["chat"]);
    }

    #[test]
    fn reads_a_fivem_manifest() {
        let manifest = parse(
            r#"
            description 'chat management stuff'
            ui_page 'html/index.html'
            client_script 'cl_chat.lua'
            server_script 'sv_chat.lua'
            shared_script 'shared.lua'
            files { 'html/index.html', 'html/App.js' }
            chat_theme 'gtao' { styleSheet = 'style.css' }
            this_is_a_map 'yes'
            "#,
        )
        .unwrap();
        assert_eq!(manifest.client_scripts, vec!["cl_chat.lua", "shared.lua"]);
        assert_eq!(manifest.server_scripts, vec!["sv_chat.lua", "shared.lua"]);
        assert_eq!(manifest.files, vec!["html/index.html", "html/App.js"]);
        assert_eq!(manifest.metadata_values("chat_theme"), vec!["gtao"]);
        assert_eq!(manifest.metadata_values("this_is_a_map"), vec!["yes"]);
    }

    #[test]
    fn the_manifest_cannot_do_anything_else() {
        assert!(parse("os.execute('echo hi')").is_err());
        assert!(parse("io.open('x')").is_err());
    }
}
