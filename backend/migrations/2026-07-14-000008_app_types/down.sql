ALTER TABLE apps DROP CONSTRAINT IF EXISTS apps_app_type_check;
ALTER TABLE apps ADD CONSTRAINT apps_app_type_check
  CHECK (app_type IN ('web','flutter','ios','android','react_native','node'));
