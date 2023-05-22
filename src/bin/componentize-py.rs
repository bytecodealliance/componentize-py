use {anyhow::Result, std::env};

fn main() -> Result<()> {
    componentize_py::command::run(env::args_os())
}
