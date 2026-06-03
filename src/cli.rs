use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "via")]
#[command(version)]
#[command(about = "Run services across machines you own")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, hide = true)]
        mesh_id: Option<String>,
        #[arg(long, hide = true)]
        node_id: Option<String>,
    },
    Doctor,
    Add {
        ssh_host: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        public: bool,
    },
    Start {
        #[arg(long, default_value = "0.0.0.0:47819")]
        bind: String,
    },
    Nodes,
    Hub {
        #[command(subcommand)]
        command: HubCommand,
    },
    Invite {
        #[command(subcommand)]
        command: InviteCommand,
    },
    Join {
        token: String,
        #[arg(long)]
        name: Option<String>,
    },
    Node {
        #[command(subcommand)]
        command: NodeCommand,
    },
    Ps,
    Deploy {
        target: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        port: Option<String>,
        #[arg(long, value_enum, default_value = "auto", hide = true)]
        route: RouteMode,
        #[arg(last = true)]
        command: Vec<String>,
    },
    Services,
    Status {
        service: String,
    },
    Logs {
        service: Option<String>,
        #[arg(long, short)]
        follow: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Stop {
        service: String,
    },
    Restart {
        service: String,
    },
    Rm {
        service: String,
    },
    Exec {
        node: String,
        #[arg(long, value_enum, default_value = "auto", hide = true)]
        route: RouteMode,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    Move {
        from: String,
        to: String,
    },
    Open {
        service: String,
    },
    Update {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        node: Option<String>,
        #[arg(long)]
        version: Option<String>,
    },
    Publish {
        service: String,
        #[arg(long)]
        private: bool,
    },
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    Daemon {
        #[arg(long, default_value = "0.0.0.0:47819")]
        bind: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum NodeCommand {
    Rename {
        old: String,
        new: String,
    },
    Addr {
        node: String,
        address: String,
    },
    Ping {
        node: String,
        #[arg(long, value_enum, default_value = "auto", hide = true)]
        route: RouteMode,
    },
    Rm {
        node: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum HubCommand {
    Use {
        url: String,
    },
    Status,
    List,
    Drop,
    Start {
        #[arg(long, default_value = "127.0.0.1:47820")]
        bind: String,
        #[arg(long)]
        lux_dir: Option<String>,
        #[arg(long)]
        migrate: bool,
    },
    Migrate {
        #[arg(long)]
        lux_dir: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum InviteCommand {
    Create {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 86400)]
        ttl: i64,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RouteMode {
    Auto,
    Direct,
    Hub,
}

#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    Set {
        name: String,
        #[arg(long)]
        value: String,
    },
    Delete {
        name: String,
    },
    List,
}
