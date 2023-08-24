use {anyhow::Result, std::env};

fn main() -> Result<()> {
    pretty_env_logger::init_timed();
    componentize_py::command::run(env::args_os())
}
