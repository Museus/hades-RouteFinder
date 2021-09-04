mod rng;
mod luabins;
mod read;
mod save;
use save::UncompressedSize;
use rng::SggPcg;
use rand::RngCore;
use structopt::StructOpt;
use rlua::{Lua, Variadic, Value};
use std::fs;
use std::rc::Rc;
use std::cell::RefCell;
use libm::ldexp;
use lz4;
use std::path::Path;
use std::sync::Arc;

#[derive(StructOpt)]
struct Cli {
  #[structopt(short = "s", long, env)]
  hades_scripts_dir: std::path::PathBuf,
  #[structopt(short = "f", long)]
  hades_save_file: std::path::PathBuf,
  #[structopt(parse(from_os_str))]
  script: std::path::PathBuf
}

#[derive(Debug)]
enum Error {
  Lua {
    error: rlua::Error
  },
  IO {
    error: std::io::Error
  }
}

type Result<T> = core::result::Result<T, Error>;

impl From<rlua::Error> for Error {
  fn from(error: rlua::Error) -> Self {
    Error::Lua { error: error }
  }
}

impl From<std::io::Error> for Error {
  fn from(error: std::io::Error) -> Self {
    Error::IO { error: error }
  }
}

impl From<Error> for rlua::Error {
  fn from(error: Error) -> Self {
     match error {
       Error::Lua { error } => error,
       Error::IO { error } => rlua::Error::ExternalError(Arc::new(error))
     }
  }
}

fn main() -> Result<()> {
    let args = Cli::from_args();
    let lua = unsafe {
      Lua::new_with_debug()
    };
    let shared_rng = Rc::new(RefCell::new(SggPcg::new(0)));
    let parent_path = args.hades_scripts_dir.clone();
    lua.context(|lua_ctx| {
        lua_ctx.scope(|scope| {
            let import = scope.create_function(|inner_lua_ctx, import_str: String| {
                let import_file = read_file(parent_path.clone().join(import_str))?;
                inner_lua_ctx.load(&import_file).exec()
            })?;
            lua_ctx.globals().set("Import", import)?;
            // Engine callbacks etc.
            let engine = read_file("Engine.lua")?;
            lua_ctx.load(&engine).exec()?;
            // Hooks into the engine for RNG
            let randomseed = scope.create_function(|_, (o_seed, _id): (Option<i32>, Value) | {
                let seed = match o_seed {
                    Some(s) => s,
                    None => 0
                };
                let mut rng = shared_rng.borrow_mut(); 
                *rng = SggPcg::new(seed as u64);
                Ok(())
            })?;
            lua_ctx.globals().set("randomseed", randomseed)?;
            let randomint = scope.create_function(|_, (min, max, _id): (i32, i32, Value)| {
                let mut rng = shared_rng.borrow_mut();
                Ok(rand_int(&mut *rng, min, max))
            })?;
            lua_ctx.globals().set("randomint", randomint)?;
            let random = scope.create_function(|_, _args: Variadic<Value>| {
                let mut rng = shared_rng.borrow_mut();
                Ok(rand_double(&mut *rng))
            })?;
            lua_ctx.globals().set("random", random)?;
            let randomgaussian = scope.create_function(|_, _args: Variadic<Value>| {
                Ok(0.0) // only affects enemy ratios in encounters, but not number of waves or types
            })?;
            lua_ctx.globals().set("randomgaussian", randomgaussian)?;
            // Load lua files
            let mut main_path = args.hades_scripts_dir.clone();
            main_path.push("Main.lua");
            let main = read_file(main_path)?;
            lua_ctx.load(&main).exec()?;
            let mut room_manager_path = args.hades_scripts_dir.clone();
            room_manager_path.push("RoomManager.lua");
            let room_manager = read_file(room_manager_path)?;
            lua_ctx.load(&room_manager).exec()?;
            let save_file = read_file(args.hades_save_file)?;
            let lua_state_lz4 = match save::read(&mut save_file.as_slice(), "save".to_string()) {
              Ok(save_file) => save_file.lua_state_lz4,
              Err(s) => {
                println!("error reading save: {}", s);
                Vec::new()
              }
            };
            let lua_state = match lz4::block::decompress(&lua_state_lz4.as_slice(), Some(save::HadesSaveV16::UNCOMPRESSED_SIZE)) {
              Ok(uncompressed) => {
                uncompressed
              },
              Err(e) => {
                println!("{}", e);
                Vec::new()
              }
            };
            match luabins::load(&mut lua_state.as_slice(), lua_ctx, "luabins".to_string()) {
              Ok(vec) => lua_ctx.globals().set("RouteFinderSaveFileData", vec)?,
              Err(s) => println!("{}", s)
            };
            // put save file data into globals
            lua_ctx.load(r#"
                for _,savedValues in pairs(RouteFinderSaveFileData) do
                  for key, value in pairs(savedValues) do
                    if not SaveIgnores[key] then
                      _G[key] = value
                    end
                  end
                end
                "#).exec()?;
            // load and run script
            let script = read_file(args.script)?;
            lua_ctx.load(&script).exec()
        })?;
        Ok(())
    })
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


fn rand_int(rng: &mut SggPcg, min: i32, max: i32) -> i32 {
  if max > min {
    let bound = (max as u32).wrapping_sub(min as u32).wrapping_add(1);
    min.wrapping_add(bounded(rng, bound) as i32)
  } else {
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

/* Rough stab at how random gaussian generate works in the Hades code.
   - seems to be an independant SggPcg used only for gaussians
   - the gaussian pcg isn't reseeded on RandomSeed or reset on RandomSynchronize
   - it does seem to be reset to the same value every time when starting the game

struct GaussState {
  has_value: bool,
  value: f64
}

fn rand_gauss(rng: &mut SggPcg, state: &mut GaussState) -> f64 {
  if state.has_value {
      state.has_value = false;
      state.value
   } else {
      let mut u: f64 = 0.0;
      let mut v: f64 = 0.0;
      let mut s: f64 = 0.0;

      // Box-Muller, polar form
      while s >= 1.0 || s == 0.0 {
        u = 2.0 * rand_double(rng) - 1.0;
        v = 2.0 * rand_double(rng) - 1.0;
        s = u * u + v * v;
      }

      let f = libm::sqrt(-2.0 * libm::log(s) / s);
      state.has_value = true; // keep for next call
      state.value = f * u;
      f * v
  }
}
*/
