UPDATE roles SET permissions = permissions - 'source:read'
WHERE name IN ('Owner', 'Admin', 'Developer');
