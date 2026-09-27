-- dogtag: per-player API keys + session history, stored in Supabase Postgres.
-- Run once in the Supabase dashboard: SQL Editor > New query > paste this > Run.
--
-- Auth model:
-- - `api_keys`: one row per generated key, owned by the authenticated user (auth.uid()). The web app
--   inserts/reads/deletes its own rows directly (RLS-protected) - the key value itself is what goes
--   into the player's config.toml.
-- - `sessions`: one row per dogtag session, written ONLY by serve-stats using the service_role key
--   (which bypasses RLS entirely - that's by design, it's how a trusted server writes on behalf of
--   whichever user_id a valid API key resolved to). Players can only SELECT their own rows from the
--   web app; there is deliberately no insert/update/delete policy for them.

create table if not exists public.api_keys (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  key text not null unique,
  label text,
  created_at timestamptz not null default now()
);

alter table public.api_keys enable row level security;

create policy "select own keys" on public.api_keys
  for select using (auth.uid() = user_id);
create policy "insert own keys" on public.api_keys
  for insert with check (auth.uid() = user_id);
create policy "delete own keys" on public.api_keys
  for delete using (auth.uid() = user_id);

create table if not exists public.sessions (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  player text not null,
  balance_now bigint,
  balance_start bigint,
  rank integer,
  started_at timestamptz not null,
  ended_at timestamptz,
  minutes numeric,
  created_at timestamptz not null default now()
);

alter table public.sessions enable row level security;

create policy "select own sessions" on public.sessions
  for select using (auth.uid() = user_id);

create index if not exists sessions_user_started_idx
  on public.sessions (user_id, started_at desc);
