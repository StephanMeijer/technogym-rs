//! Hosts and the client identity mywellness expects.

use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};

/// Account and login host.
pub const CORE_URL: &str = "https://core.mywellness.com";
/// Training-data host; every path under it starts with the facility's url slug.
pub const SERVICES_URL: &str = "https://services.mywellness.com";
/// The login endpoint under [`CORE_URL`].
pub const LOGIN_PATH: &str = "/v2/enduser/authentication/login";

/// App id of the official end-user web app. Without it the API answers
/// `ClientApplicationNotTrusted`.
pub const APP_ID: &str = "EC1D38D7-D359-48D0-A60C-D8C0B8FB9DF9";
/// App name of the official end-user web app.
pub const APP_NAME: &str = "enduserweb";
/// App version reported alongside [`APP_NAME`].
pub const APP_VERSION: &str = "1.0";
/// The `User-Agent` this crate sends.
pub const USER_AGENT_VALUE: &str = "technogym-rs (+https://github.com/StephanMeijer/technogym-rs)";

/// The headers every mywellness request carries.
pub fn app_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(ACCEPT, HeaderValue::from_static("application/json"));
    h.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    h.insert("x-mwapps-appid", HeaderValue::from_static(APP_ID));
    h.insert("x-mwapps-client", HeaderValue::from_static(APP_NAME));
    h.insert(
        "x-mwapps-clientversion",
        HeaderValue::from_static("1.0,enduserweb"),
    );
    h
}
