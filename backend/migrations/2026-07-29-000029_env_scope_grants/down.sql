-- Env grants must go BEFORE the narrower constraint is restored, or this
-- migration fails against its own data. This deletes access; it is a
-- destructive rollback by necessity, not by choice.
DELETE FROM role_grants WHERE scope_type = 'env';

ALTER TABLE role_grants DROP CONSTRAINT role_grants_scope_type_check;
ALTER TABLE role_grants
    ADD CONSTRAINT role_grants_scope_type_check
    CHECK (scope_type IN ('org', 'project', 'app'));
