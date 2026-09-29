#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AccountFlow {
    #[default]
    SignIn,
    Register,
    VerifyEmail,
    RequestPasswordReset,
    ResetPassword,
}
