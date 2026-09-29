-- Read-only administrator dashboard persistence. Aggregate traffic contains
-- only bounded route categories and measurements; exact client addresses are
-- held separately under the shorter retention policy below. Raw URLs, account
-- identifiers, headers, and request bodies are never stored.

CREATE TABLE admin_traffic_minute (
    bucket timestamp with time zone NOT NULL,
    route_group text NOT NULL,
    method text NOT NULL,
    status_class smallint NOT NULL CHECK (status_class BETWEEN 1 AND 5),
    request_count bigint NOT NULL CHECK (request_count >= 0),
    request_bytes bigint NOT NULL CHECK (request_bytes >= 0),
    response_bytes bigint NOT NULL CHECK (response_bytes >= 0),
    total_duration_ms bigint NOT NULL CHECK (total_duration_ms >= 0),
    max_duration_ms bigint NOT NULL CHECK (max_duration_ms >= 0),
    PRIMARY KEY (bucket, route_group, method, status_class)
);

CREATE INDEX idx_admin_traffic_minute_bucket ON admin_traffic_minute USING btree (bucket DESC);

-- Exact client addresses are retained briefly for incident response. The
-- transport decides whether the direct peer or Caddy's canonical forwarded
-- address is trusted; no arbitrary forwarding chain is stored.
CREATE TABLE admin_request_ip (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at timestamp with time zone NOT NULL,
    client_ip inet NOT NULL,
    route_group text NOT NULL,
    method text NOT NULL,
    status_class smallint NOT NULL CHECK (status_class BETWEEN 1 AND 5),
    request_bytes bigint NOT NULL CHECK (request_bytes >= 0),
    response_bytes bigint NOT NULL CHECK (response_bytes >= 0),
    duration_ms bigint NOT NULL CHECK (duration_ms >= 0)
);

CREATE INDEX idx_admin_request_ip_time ON admin_request_ip USING btree (occurred_at DESC);
CREATE INDEX idx_admin_request_ip_address_time ON admin_request_ip USING btree (client_ip, occurred_at DESC);

-- One bounded row per account records successful authenticated use. Updates
-- are batched by the account service so this does not add a write per request.
CREATE TABLE admin_account_activity (
    user_id text PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    first_seen_at timestamp with time zone NOT NULL,
    last_seen_at timestamp with time zone NOT NULL,
    request_count bigint NOT NULL CHECK (request_count > 0),
    CHECK (last_seen_at >= first_seen_at)
);

CREATE INDEX idx_admin_account_activity_last_seen ON admin_account_activity USING btree (last_seen_at DESC);

-- Daily rows make engagement trends queryable without retaining a request
-- history or exposing account identities through the administrator API. The
-- account worker expires rows after 90 days.
CREATE TABLE admin_account_activity_day (
    user_id text NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    activity_date date NOT NULL,
    request_count bigint NOT NULL CHECK (request_count >= 0),
    sync_request_count bigint NOT NULL CHECK (sync_request_count >= 0),
    CHECK (request_count + sync_request_count > 0),
    PRIMARY KEY (user_id, activity_date)
);

CREATE INDEX idx_admin_account_activity_day_date ON admin_account_activity_day USING btree (activity_date DESC);

CREATE TABLE admin_audit_event (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    administrator_user_id text NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    event_type text NOT NULL,
    window_started_at timestamp with time zone NOT NULL,
    occurred_at timestamp with time zone NOT NULL DEFAULT now()
);

CREATE INDEX idx_admin_audit_event_time ON admin_audit_event USING btree (occurred_at DESC);
CREATE UNIQUE INDEX idx_admin_audit_event_window ON admin_audit_event USING btree (administrator_user_id, event_type, window_started_at);

CREATE FUNCTION reject_admin_audit_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'administrator audit events are append-only';
END;
$$;

CREATE TRIGGER admin_audit_event_append_only
BEFORE UPDATE OR DELETE ON admin_audit_event
FOR EACH ROW EXECUTE FUNCTION reject_admin_audit_mutation();
