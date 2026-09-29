use super::SyncPage;
use crate::android_authentication::Session;
use gpui::Context;

pub(super) struct AndroidAuthentication {
    _session: Session,
    _task: gpui::Task<()>,
}
impl SyncPage {
    pub(super) fn open_android_authentication(&mut self, cx: &mut Context<Self>) {
        self.android_authentication = None;
        let theme = ui_components::browser_theme(cx);
        let colors = [theme.page_bg, theme.text, theme.text_muted, theme.accent, theme.accent_text]
            .into_iter()
            .map(|color| {
                let c = color.to_rgb();
                0xff000000 | ((c.r * 255.0).round() as u32) << 16 | ((c.g * 255.0).round() as u32) << 8 | (c.b * 255.0).round() as u32
            })
            .collect();
        let (session, requests) = match Session::open(colors) {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        let id = session.0;
        let backend = self.services.backend.clone();
        let task = cx.spawn(async move |page, cx| {
            while let Ok(request) = requests.recv().await {
                let action = request.action.as_str();
                if action == "closed" {
                    break;
                }
                let result = match action {
                    "login" => backend.login(request.email, request.secret).await.map(|_| ()),
                    "register" => backend.request_public_registration(request.email, request.secret).await,
                    "verify" => backend.verify_email(request.email, request.secret).await.map(|_| ()),
                    "resend" => backend.resend_verification(request.email).await,
                    "request-reset" => backend.request_password_reset(request.email).await,
                    "reset" => backend.reset_password(request.token, request.secret).await,
                    _ => Err("Unknown authentication action".into()),
                };
                let signed_in = result.is_ok() && matches!(action, "login" | "verify");
                Session::complete(id, result.err());
                if signed_in {
                    let _ = page.update(cx, |page, cx| page.schedule_refresh(cx));
                }
            }
        });
        self.android_authentication = Some(AndroidAuthentication { _session: session, _task: task });
    }
}
