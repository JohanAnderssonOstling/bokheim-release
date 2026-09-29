-- Account/auth domain: registration, sessions, tokens, and password reset.
-- Depends only on 01_core.sql (users).

-- message_kind carries no CHECK: it is only ever written from the exhaustive
-- AccountEmailKind Rust enum (server-account::core). attempts keeps its
-- CHECK because it is incremented in raw SQL ("attempts=attempts+1" in
-- outbox.rs), which Rust's type system cannot guard against underflowing.
CREATE TABLE account_email_outbox (
    id text PRIMARY KEY,
    recipient text NOT NULL,
    message_kind text NOT NULL,
    token_ciphertext text NOT NULL,
    created_at timestamp with time zone NOT NULL DEFAULT now(),
    available_at timestamp with time zone NOT NULL DEFAULT now(),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claimed_by text,
    claim_expires_at timestamp with time zone,
    last_error text
);

CREATE INDEX idx_account_email_outbox_available ON account_email_outbox USING btree (available_at, created_at);

CREATE TABLE account_rate_limit (
    bucket_key text PRIMARY KEY,
    window_started_at timestamp with time zone NOT NULL,
    attempts integer NOT NULL CHECK (attempts > 0)
);

CREATE INDEX idx_account_rate_limit_window ON account_rate_limit USING btree (window_started_at);

CREATE TABLE auth_session (
    id text PRIMARY KEY,
    user_id text NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    refresh_token_hash text NOT NULL UNIQUE,
    created_at timestamp with time zone NOT NULL DEFAULT now(),
    refresh_expires_at timestamp with time zone NOT NULL
);

CREATE INDEX idx_auth_session_expiry ON auth_session USING btree (refresh_expires_at);
CREATE INDEX idx_auth_session_user ON auth_session USING btree (user_id);

CREATE TABLE auth_consumed_refresh_token (
    token_hash text PRIMARY KEY,
    session_id text NOT NULL REFERENCES auth_session(id) ON DELETE CASCADE,
    consumed_at timestamp with time zone NOT NULL DEFAULT now(),
    expires_at timestamp with time zone NOT NULL
);

CREATE INDEX idx_auth_consumed_refresh_token_expiry ON auth_consumed_refresh_token USING btree (expires_at);
CREATE INDEX idx_auth_consumed_refresh_token_session ON auth_consumed_refresh_token USING btree (session_id);

-- event_type carries no CHECK: 'refresh_token_reuse' is the only value any
-- Rust call site ever writes here.
CREATE TABLE auth_security_event (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id text NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    session_id text NOT NULL,
    event_type text NOT NULL,
    detected_at timestamp with time zone NOT NULL DEFAULT now()
);

CREATE INDEX idx_auth_security_event_user_time ON auth_security_event USING btree (user_id, detected_at DESC);

CREATE TABLE auth_token (
    token_hash text PRIMARY KEY,
    user_id text NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at timestamp with time zone NOT NULL DEFAULT now(),
    expires_at timestamp with time zone NOT NULL,
    session_id text REFERENCES auth_session(id) ON DELETE CASCADE
);

CREATE INDEX idx_auth_token_expiry ON auth_token USING btree (expires_at);
CREATE UNIQUE INDEX idx_auth_token_session ON auth_token USING btree (session_id) WHERE (session_id IS NOT NULL);
CREATE INDEX idx_auth_token_user ON auth_token USING btree (user_id);

CREATE TABLE password_reset (
    user_id text PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    token_hash text NOT NULL UNIQUE,
    requested_at timestamp with time zone NOT NULL DEFAULT now(),
    expires_at timestamp with time zone NOT NULL
);

CREATE INDEX idx_password_reset_expiry ON password_reset USING btree (expires_at);

CREATE TABLE pending_registration (
    id text PRIMARY KEY,
    email text NOT NULL UNIQUE,
    password_hash text NOT NULL,
    verification_hash text NOT NULL,
    created_at timestamp with time zone NOT NULL DEFAULT now(),
    expires_at timestamp with time zone NOT NULL,
    last_sent_at timestamp with time zone NOT NULL DEFAULT now()
);

CREATE INDEX idx_pending_registration_expiry ON pending_registration USING btree (expires_at);
