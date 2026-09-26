//! WiFi: joins the network named in `.cargo/config.toml` and runs the network
//! stack. `start` returns the stack; hand it to branches that open sockets.

use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ControllerConfig, Interface, WifiController,
};
use static_cell::StaticCell;

/// The network to join. Set both in `.cargo/config.toml`, or export them when
/// you build to keep the password out of git.
const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

/// Sockets open at once. DHCP and DNS use two; your branches share the rest.
const SOCKETS: usize = 6;

/// Bring up WiFi and the network stack. The trunk calls this once at startup.
pub fn start(spawner: &Spawner, wifi: WIFI<'static>) -> Stack<'static> {
    let station = StationConfig::default()
        .with_ssid(SSID.try_into().unwrap_or_else(|_| panic!("wifi: WIFI_SSID is too long")))
        .with_authentication(if PASSWORD.is_empty() {
            AuthenticationMethodConfig::Open
        } else {
            AuthenticationMethodConfig::Wpa2Personal(
                PASSWORD
                    .try_into()
                    .unwrap_or_else(|_| panic!("wifi: WIFI_PASSWORD is too long")),
            )
        });
    let controller = WifiController::new(
        wifi,
        ControllerConfig::default().with_initial_config(Config::Station(station)),
    )
    .unwrap_or_else(|_| panic!("wifi: can't start the radio"));

    // DHCP picks the address; the seed randomizes the stack's port numbers.
    static RESOURCES: StaticCell<StackResources<SOCKETS>> = StaticCell::new();
    let seed = u64::from(Rng::new().random()) << 32 | u64::from(Rng::new().random());
    let (stack, runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );

    // Fails only if these are already running. A fixed message links no formatting code.
    spawner.spawn(net(runner).unwrap_or_else(|_| panic!("wifi: net already running")));
    spawner.spawn(link(controller, stack).unwrap_or_else(|_| panic!("wifi: link already running")));
    stack
}

/// Keeps the station connected: joins, waits for a drop, rejoins.
#[embassy_executor::task]
async fn link(mut controller: WifiController<'static>, stack: Stack<'static>) {
    if SSID.is_empty() {
        defmt::warn!("wifi: no network set; put WIFI_SSID in .cargo/config.toml");
        return;
    }
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                defmt::info!("wifi: joined {}", SSID);
                stack.wait_config_up().await;
                if let Some(config) = stack.config_v4() {
                    defmt::info!("wifi: address {}", config.address);
                }
                let _ = controller.wait_for_disconnect_async().await;
                defmt::warn!("wifi: disconnected");
            }
            Err(e) => defmt::warn!("wifi: can't join {}: {}", SSID, e),
        }
        Timer::after(Duration::from_secs(5)).await; // don't hammer the access point
    }
}

/// Runs the network stack: moves packets between the radio and the sockets.
#[embassy_executor::task]
async fn net(mut runner: Runner<'static, Interface>) {
    runner.run().await
}
