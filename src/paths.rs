use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ViaPaths {
    pub root: PathBuf,
    pub lux: PathBuf,
    pub logs: PathBuf,
    pub bin: PathBuf,
    pub mesh_key: PathBuf,
}

impl ViaPaths {
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("could not find home directory")?;
        let root = home.join(".via");
        Ok(Self {
            lux: root.join("lux"),
            logs: root.join("logs"),
            bin: root.join("bin"),
            mesh_key: root.join("mesh.key"),
            root,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(&self.lux)?;
        std::fs::create_dir_all(&self.logs)?;
        std::fs::create_dir_all(&self.bin)?;
        Ok(())
    }
}
