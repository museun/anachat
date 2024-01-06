mod channel;
mod display_channel;
mod geometry;
mod model;
mod root_view;
mod tab;
mod tabs;
mod twitch;

fn main() -> anyhow::Result<()> {
    simple_env_load::load_env_from([".secrets.env", ".dev.env"]);
    let config = twitch::Config::from_env()?;

    anathema::core::Factory::register("tab", tab::TabFactory)?;

    let (req_tx, req_rx) = smol::channel::unbounded();
    let (resp_tx, resp_rx) = smol::channel::unbounded();

    let handle = std::thread::spawn(move || twitch::connect(config, req_rx, resp_tx));

    let root_view = root_view::RootView {
        state: root_view::RootState::default(),
        tabs: tabs::Tabs::default(),
        feed: resp_rx,
        send: req_tx.clone(),
    };

    let template = std::fs::read_to_string("templates/root.aml")?;
    let mut templates = anathema::vm::Templates::new(template, root_view);
    let templates = templates.compile()?;

    let mut runtime = anathema::runtime::Runtime::new(&templates)?;
    runtime.enable_alt_screen = false;

    runtime.run()?;

    // lets ensure the thread ends, we don't care if we can't send to it
    let _ = req_tx.send_blocking(twitch::Request::Disconnect { reconnect: false });

    handle.join().unwrap()
}
