use crate::state_types::Event::*;
use crate::state_types::Internal::*;
use crate::state_types::*;
use crate::types::addons::Descriptor;
use crate::types::api::*;
use derivative::*;
use futures::Future;
use lazy_static::*;
use serde_derive::*;
use std::collections::HashMap;
use std::marker::PhantomData;

const USER_DATA_KEY: &str = "userData";
lazy_static! {
    static ref DEFAULT_ADDONS: Vec<Descriptor> = serde_json::from_slice(include_bytes!(
        "../../../stremio-official-addons/index.json"
    ))
    .expect("official addons JSON parse");
}

///////////////////////////
/// TODO Fix these
/// and use snake_case and serde option to convert it to camelCase
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SsOption {
    pub id: String,
    pub label: String,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SsValues {
        appPath: String,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SsSettings {
    options: Vec<SsOption>,
    values: SsValues,
    baseUrl: String
}
///////////////////////////


// These will be stored, so they need to implement both Serialize and Deserilaize
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Auth {
    pub key: AuthKey,
    pub user: User,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CtxContent {
    pub auth: Option<Auth>,
    pub addons: Vec<Descriptor>,
    pub settings: HashMap<String, String>,
}
impl Default for CtxContent {
    fn default() -> Self {
        let settings: HashMap<String, String> = [
            ("language", "eng"),
            ("subtitles_size", "100%"),
            ("subtitles_language", "eng"),
            ("subtitles_background", ""),
            ("subtitles_color", "#fff"),
            ("subtitles_outline_color", "#000"),
            ("autoplay_next_vid", "true"),
            ("server_url", "http://127.0.0.1:11470/"),
            ("use_external_player", "false"),
            // We can't override Esc in browser so this option is pointless here
            // ("player_esc_exits_fullscreen", "false"),
            ("pause_on_lost_focus", "false"),
            ("show_vid_overview", "false"),
        ]
        .iter()
        .map(|&opt| (opt.0.to_string(), opt.1.to_string()))
        .collect();

        CtxContent {
            auth: None,
            addons: DEFAULT_ADDONS.to_owned(),
            settings: settings,
        }
    }
}

#[derive(Derivative, Serialize)]
#[derivative(Debug, Default, Clone)]
pub struct Ctx<Env: Environment> {
    pub content: CtxContent,
    // Whether it's loaded from storage
    pub is_loaded: bool,
    #[serde(skip)]
    pub library: LibraryLoadable,
    #[derivative(Debug = "ignore")]
    #[serde(skip)]
    env: PhantomData<Env>,
}

fn fetch_server_settings(local_settings: HashMap<String, String>) -> Option<Request<()>> {
    match Request::get(local_settings.get("server_url")?).body(()) {
        Ok(res) => Some(res),
        Err(_) => None
    }
}

impl<Env: Environment + 'static> Update for Ctx<Env> {
    fn update(&mut self, msg: &Msg) -> Effects {
        let fx = match msg {
            // Loading from storage: request it
            Msg::Action(Action::LoadCtx) if !self.is_loaded => {
                Effects::one(load_storage::<Env>()).unchanged()
            }
            Msg::Internal(CtxLoaded(opt_content)) => {
                self.content = opt_content
                    .as_ref()
                    .map(|x| *x.to_owned())
                    .unwrap_or_default();

                    let local_settings: HashMap<String, String> = self.content.settings.iter()
                        .map(|opt| (opt.0.to_string(), opt.1.to_string()))
                        .collect();
                    match fetch_server_settings(local_settings) {
                        Some(resp) => {
                            self.content.settings.insert("fetched".to_string(), "yes".to_string());
                            // Env::fetch_serde::<_, SsSettings>(resp).and_then(|settings: SsSettings| {
                            //     self.content.settings.insert("fetched".to_string(), settings.baseUrl);
                            // });
                        },
                        None => {
                            self.content.settings.insert("fetched".to_string(), "no".to_string());
                        }
                    };
                self.is_loaded = true;
                self.library.load_from_storage::<Env>(&self.content)
            }
            // Addon install/remove
            Msg::Action(Action::AddonOp(ActionAddon::Remove { transport_url })) => {
                let pos = self
                    .content
                    .addons
                    .iter()
                    .position(|x| x.transport_url == *transport_url);
                if let Some(idx) = pos {
                    self.content.addons.remove(idx);
                    Effects::one(save_storage::<Env>(&self.content))
                } else {
                    Effects::none().unchanged()
                }
            }
            Msg::Action(Action::AddonOp(ActionAddon::Install(descriptor))) => {
                // @TODO should we dedupe?
                self.content.addons.push(*descriptor.to_owned());
                Effects::one(save_storage::<Env>(&self.content))
            }
            Msg::Action(Action::Settings(ActionSettings::Store(settings))) => {
                for (property, value) in &(*settings.to_owned()) {
                    self.content
                        .settings
                        .insert(property.to_string(), value.to_string());
                }
                Effects::one(save_storage::<Env>(&self.content))
            }
            // User actions related to API primitives (authentication/addons)
            Msg::Action(Action::UserOp(action)) => match action.to_owned() {
                ActionUser::Logout => {
                    let new_content = Box::new(CtxContent::default());
                    match &self.content.auth {
                        Some(Auth { key, .. }) => {
                            let action = action.clone();
                            let req = APIRequest::Logout {
                                auth_key: key.to_owned(),
                            };
                            let effect = api_fetch::<Env, SuccessResponse, _>(req)
                                .map(|_| CtxUpdate(new_content).into())
                                .map_err(move |e| CtxActionErr(action, e).into());
                            Effects::one(Box::new(effect)).unchanged()
                        }
                        None => Effects::msg(CtxUpdate(new_content).into()).unchanged(),
                    }
                }
                ActionUser::Register { email, password } => Effects::one(authenticate::<Env>(
                    action.to_owned(),
                    APIRequest::Register { email, password },
                ))
                .unchanged(),
                ActionUser::Login { email, password } => Effects::one(authenticate::<Env>(
                    action.to_owned(),
                    APIRequest::Login { email, password },
                ))
                .unchanged(),
                ActionUser::PullAndUpdateAddons => match &self.content.auth {
                    Some(Auth { key, .. }) => {
                        let action = action.to_owned();
                        let key = key.to_owned();
                        let req = APIRequest::AddonCollectionGet {
                            auth_key: key.to_owned(),
                            update: true,
                        };
                        // @TODO: respect last_modified
                        let ft = api_fetch::<Env, CollectionResponse, _>(req)
                            .map(move |r| CtxAddonsPulled(key, r.addons).into())
                            .map_err(move |e| CtxActionErr(action, e).into());
                        Effects::one(Box::new(ft)).unchanged()
                    }
                    None => {
                        // Local update based on the DEFAULT_ADDONS
                        self.content.addons =
                            addons_upgrade_local(&DEFAULT_ADDONS, &self.content.addons);
                        Effects::none()
                    }
                },
                ActionUser::PushAddons => match &self.content.auth {
                    Some(Auth { key, .. }) => {
                        let action = action.to_owned();
                        let req = APIRequest::AddonCollectionSet {
                            auth_key: key.to_owned(),
                            addons: self.content.addons.to_owned(),
                        };
                        let ft = api_fetch::<Env, SuccessResponse, _>(req)
                            .map(|_| CtxAddonsPushed.into())
                            .map_err(move |e| CtxActionErr(action, e).into());
                        Effects::one(Box::new(ft)).unchanged()
                    }
                    None => Effects::none().unchanged(),
                },
                // We let the LibraryLoadable model handle this
                ActionUser::LibSync | ActionUser::LibUpdate(_) => Effects::none().unchanged(),
            },
            // Handling msgs that result effects
            Msg::Internal(CtxAddonsPulled(key, addons))
                if self.content.auth.as_ref().map(|a| &a.key) == Some(&key)
                    && &self.content.addons != addons =>
            {
                self.content.addons = addons.to_owned();
                Effects::msg(CtxAddonsChangedFromPull.into())
                    .join(Effects::one(save_storage::<Env>(&self.content)))
            }
            Msg::Internal(CtxUpdate(new)) => {
                self.content = *new.to_owned();
                Effects::msg(CtxChanged.into())
                    .join(Effects::one(save_storage::<Env>(&self.content)))
                    // When doing CtxUpdate, this means we've changed authentication,
                    // so we re-load the library from the API
                    .join(self.library.load_initial::<Env>(&self.content))
            }
            _ => Effects::none().unchanged(),
        };

        fx.join(self.library.update::<Env>(&self.content, msg))
    }
}

fn load_storage<Env: Environment>() -> Effect {
    Box::new(
        Env::get_storage(USER_DATA_KEY)
            .map(|x| Msg::Internal(CtxLoaded(x)))
            .map_err(|e| Msg::Event(CtxFatal(e.into()))),
    )
}

fn save_storage<Env: Environment>(content: &CtxContent) -> Effect {
    Box::new(
        Env::set_storage(USER_DATA_KEY, Some(content))
            .map(|_| Msg::Event(CtxSaved))
            .map_err(|e| Msg::Event(CtxFatal(e.into()))),
    )
}

fn authenticate<Env: Environment + 'static>(action: ActionUser, req: APIRequest) -> Effect {
    let ft = api_fetch::<Env, AuthResponse, _>(req)
        .and_then(move |AuthResponse { key, user }| {
            let pull_req = APIRequest::AddonCollectionGet {
                auth_key: key.to_owned(),
                update: true,
            };

            // This is used only for authenticated users.
            let settings = HashMap::new();

            api_fetch::<Env, CollectionResponse, _>(pull_req).map(
                move |CollectionResponse { addons, .. }| CtxContent {
                    auth: Some(Auth { key, user }),
                    addons,
                    settings,
                },
            )
        })
        .map(|c| Msg::Internal(CtxUpdate(Box::new(c))))
        .map_err(move |e| Msg::Event(CtxActionErr(action, e)));
    Box::new(ft)
}

fn addons_upgrade_local(defaults: &[Descriptor], addons: &[Descriptor]) -> Vec<Descriptor> {
    addons
        .iter()
        .map(|addon| {
            let newer = defaults.iter().find(|x| x.manifest.id == addon.manifest.id);
            match newer {
                Some(newer) if newer.manifest.version > addon.manifest.version => Descriptor {
                    flags: addon.flags.clone(),
                    ..newer.clone()
                },
                _ => addon.clone(),
            }
        })
        .collect()
}
