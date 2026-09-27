-- Adds wins/losses to an already-created `sessions` table (schema.sql now creates them from the
-- start, but your table already exists). Run once in the Supabase SQL editor.
alter table public.sessions add column if not exists wins integer not null default 0;
alter table public.sessions add column if not exists losses integer not null default 0;
