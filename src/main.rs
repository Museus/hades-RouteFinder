mod error;
mod luabins;
mod read;
mod rng;
mod save;
use clap::Parser;
use libm::ldexp;
use lz4;
use mlua::{Lua, LuaOptions, Table, Value, Variadic};
use rand::RngCore;
use rng::SggPcg;
use save::UncompressedSize;
use std::cell::RefCell;
use std::fs;
use std::time::{Duration, SystemTime};
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    script: std::path::PathBuf,

    /// Save file to use as starting point
    #[arg(short = 'f', long, value_name = "FILE")]
    save_file: PathBuf,

    /// Hades Scripts directory
    #[arg(short = 's', long, value_name = "FILE")]
    scripts_dir: PathBuf,
}

type Result<T, E = error::Error> = core::result::Result<T, E>;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let route_finder_script = &cli.script;
    let hades_scripts_dir = &cli.scripts_dir;

    let lua = unsafe { Lua::unsafe_new_with(mlua::StdLib::ALL, LuaOptions::new()) };

    let shared_rng = Rc::new(RefCell::new(SggPcg::new(0)));

    // Load save file
    let save_file = read_file(cli.save_file)?;
    let lua_state_lz4 = save::read(&mut save_file.as_slice(), "save".to_string())?.lua_state_lz4;
    let lua_state = lz4::block::decompress(
        &lua_state_lz4.as_slice(),
        Some(save::HadesSaveV16::UNCOMPRESSED_SIZE),
    )?;

    lua.scope(|scope: &mlua::Scope| -> Result<(), mlua::Error> {
        let getmetatable = scope.create_function(|_, table: Table| {
            Ok(table.get_metatable())
        })?;
        lua.globals().set("getmetatable", getmetatable)?;

        // Engine callbacks etc.
        let _ = load_lua_file(&lua, &"Engine.lua")?;

        // Hooks into the engine for RNG
        let randomseed = scope.create_function(|_, (o_seed, id): (Option<i32>, i32)| {
            let seed = match o_seed {
                Some(s) => s,
                None => 0,
            };
            let mut rng = shared_rng.borrow_mut();
            *rng = SggPcg::new(seed as u64);
            Ok(id)
        })?;
        lua.globals().set("randomseed", randomseed)?;

        let randomint = scope.create_function(|_, (min, max, _id): (i32, i32, Value)| {
            let mut rng = shared_rng.borrow_mut();
            Ok(rand_int(&mut *rng, min, max))
        })?;
        lua.globals().set("randomint", randomint)?;

        let random = scope.create_function(|_, _args: Variadic<Value>| {
            let mut rng = shared_rng.borrow_mut();
            Ok(rand_double(&mut *rng))
        })?;
        lua.globals().set("random", random)?;

        let randomgaussian = scope.create_function(|_, _args: Variadic<Value>| {
            Ok(0.0) // only affects enemy ratios in encounters, but not number of waves or types
        })?;
        lua.globals().set("randomgaussian", randomgaussian)?;


        // Load lua files
        load_lua_file(&lua, &hades_scripts_dir.join("Main.lua"))?;
        load_lua_file(&lua, &hades_scripts_dir.join("RoomManager.lua"))?;

        let save_data = luabins::load(&lua, &mut lua_state.as_slice(), "luabins".to_string()).unwrap();

        lua.globals().set("RouteFinderSaveFileData", save_data)?;

        // put save file data into globals
        lua.load(
            r#"
            for _,savedValues in pairs(RouteFinderSaveFileData) do
                for key, value in pairs(savedValues) do
                if not SaveIgnores[key] then
                    _G[key] = value
                end
                end
            end
            "#,
        )
        .exec()?;


        let deepcopytable = scope.create_function(|_, original: Value| {
            deep_copy_table(&lua, original)
        })?;
        lua.globals().set("DeepCopyTable", deepcopytable)?;

        let now: SystemTime = SystemTime::now();

        // load and run script
        let _ = load_lua_file(&lua, route_finder_script);

        match now.elapsed() {
            Ok(elapsed) => {
                // it prints '2'
                println!("Took {} seconds.", elapsed.as_millis() as f32 / 1000.0);
            }
            Err(e) => {
                // an error occurred!
                println!("Error while timing: {e:?}");
            }
        }
        Ok(())
    })?;

    Ok(())
}

const BYTE_ORDER_MARK: &[u8] = "\u{feff}".as_bytes();
fn read_file<P: AsRef<Path>>(path: P) -> Result<Vec<u8>> {
    let file = fs::read(path)?;
    if file.starts_with(BYTE_ORDER_MARK) {
        Ok(file[3..].to_vec())
    } else {
        Ok(file.to_vec())
    }
}

fn load_lua_file<'lua, P: AsRef<Path>>(lua: &Lua, path: &P) -> Result<(), mlua::Error> {
    let abs_path = path.as_ref().canonicalize()?;
    let parent_path = abs_path.parent().ok_or("No parent path".to_string()).unwrap();

    lua.scope(|scope| {
        let import = scope.create_function(|inner_lua, import_str: String| {
            let import_n1 = import_str.clone();
            let import_n2 = import_str.clone();
            let import_file = read_file(parent_path.join(import_n1))?;
            inner_lua.load(&import_file).set_name(&import_n2).exec()
        })?;

        let file = read_file(path)?;
        lua.globals().set("Import", import)?;
        lua.load(&file).exec()?;
        Ok(())
    })
}

fn rand_int(rng: &mut SggPcg, min: i32, max: i32) -> i32 {
    // println!("rand_int min {} max {}", min, max);
    if max > min {
        let bound = (max as u32).wrapping_sub(min as u32).wrapping_add(1);
        min.wrapping_add(bounded(rng, bound) as i32)
    } else {
        rng.next_u32(); // advance and ignore result (to keep in sync)
        min
    }
}

fn bounded(rng: &mut SggPcg, bound: u32) -> u32 {
    let threshold = (u32::MAX - bound + 1) % bound;

    loop {
        let r = rng.next_u32();
        if r >= threshold {
            return r % bound;
        }
    }
}

fn rand_double(rng: &mut SggPcg) -> f64 {
    ldexp(rng.next_u32() as f64, -32)
}

fn deep_copy_table<'lua>(lua: &'lua Lua, original: Value<'lua>) -> Result<Value<'lua>, mlua::Error> {
    if original.is_table() {
        let copy: Table = lua.create_table()?;
        if let Some(original_table) = original.as_table() {
            original_table.for_each::<Value, Value>(|key, value| {
                copy.set(key, deep_copy_table(&lua, value)?)
            })?;
        }

        Ok(mlua::Value::Table(copy))
    } else {
        Ok(original.clone())
    }
}
