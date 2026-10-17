//! Deterministic authorization and request-bound second-person review.
use crate::store::{Store, Work};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

// A non-WhatsApp principal used only by the authenticated web request endpoint.
const WEB_ADMIN_REQUESTER: &str = "carbon:web-admin-requests";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    CreateCoordinationScope {
        requesters: String,
        recipients: String,
        #[serde(default)]
        resources: bool,
    },
    SetCoordinationResources {
        scope: String,
        enabled: bool,
    },
    SetCoordinationScope {
        scope: String,
        enabled: bool,
    },
    AddApprovalAdmin {
        user: String,
    },
    RemoveApprovalAdmin {
        user: String,
    },
    MoveResource {
        resource: String,
        parent: Option<String>,
    },
    CreateGroup {
        name: String,
    },
    AddMember {
        group: String,
        user: String,
    },
    RemoveMember {
        group: String,
        user: String,
    },
    SetGroupAdmin {
        group: String,
        enabled: bool,
    },
    GrantRead {
        resource: String,
        audience: Audience,
    },
    RevokeRead {
        resource: String,
        audience: Audience,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Audience {
    User { id: String },
    Group { id: String },
    Everyone,
}
/// Match the tagged Rust enums exactly, including under Responses strict-mode
/// normalization. A superset object invites irrelevant fields that parsing rejects.
pub(crate) fn change_parameters() -> Value {
    fn object(properties: Value) -> Value {
        let required: Vec<_> = properties.as_object().unwrap().keys().cloned().collect();
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }
    let string = json!({"type":"string"});
    let boolean = json!({"type":"boolean"});
    let audience = json!({"anyOf":[
        object(json!({"kind":{"type":"string","enum":["user"]},"id":string})),
        object(json!({"kind":{"type":"string","enum":["group"]},"id":string})),
        object(json!({"kind":{"type":"string","enum":["everyone"]}}))
    ]});
    let shapes = [
        (
            "create_coordination_scope",
            json!({"requesters":string,"recipients":string,"resources":boolean}),
        ),
        (
            "set_coordination_scope",
            json!({"scope":string,"enabled":boolean}),
        ),
        (
            "set_coordination_resources",
            json!({"scope":string,"enabled":boolean}),
        ),
        ("create_group", json!({"name":string})),
        ("add_approval_admin", json!({"user":string})),
        ("remove_approval_admin", json!({"user":string})),
        ("add_member", json!({"group":string,"user":string})),
        ("remove_member", json!({"group":string,"user":string})),
        ("set_group_admin", json!({"group":string,"enabled":boolean})),
        ("grant_read", json!({"resource":string,"audience":audience})),
        (
            "revoke_read",
            json!({"resource":string,"audience":audience}),
        ),
        (
            "move_resource",
            json!({"resource":string,"parent":{"type":["string","null"]}}),
        ),
    ];
    let variants: Vec<_> = shapes
        .into_iter()
        .map(|(op, mut fields)| {
            fields
                .as_object_mut()
                .unwrap()
                .insert("op".into(), json!({"type":"string","enum":[op]}));
            object(fields)
        })
        .collect();
    object(json!({"change":{"anyOf":variants}}))
}

impl Audience {
    fn key(&self) -> (&str, &str) {
        match self {
            Self::User { id } => ("user", id),
            Self::Group { id } => ("group", id),
            Self::Everyone => ("everyone", "*"),
        }
    }
}
pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS access_groups(id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE COLLATE NOCASE,owner TEXT NOT NULL REFERENCES users(id),admin INTEGER NOT NULL DEFAULT 0 CHECK(admin IN(0,1)),revision INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS approval_admin_generations(user_id TEXT PRIMARY KEY REFERENCES users(id),generation INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS memberships(group_id TEXT NOT NULL REFERENCES access_groups(id),user_id TEXT NOT NULL REFERENCES users(id),PRIMARY KEY(group_id,user_id));
    CREATE TABLE IF NOT EXISTS read_grants(resource TEXT NOT NULL REFERENCES resources(id),kind TEXT NOT NULL CHECK(kind IN('user','group','everyone')),principal TEXT NOT NULL,PRIMARY KEY(resource,kind,principal));
    CREATE TABLE IF NOT EXISTS resource_acl_versions(resource TEXT PRIMARY KEY REFERENCES resources(id),revision INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS resource_sources(resource TEXT NOT NULL REFERENCES resources(id),source TEXT NOT NULL REFERENCES resources(id),PRIMARY KEY(resource,source),CHECK(resource<>source));
    CREATE TABLE IF NOT EXISTS context_sources(user_id TEXT NOT NULL REFERENCES users(id),resource TEXT NOT NULL REFERENCES resources(id),PRIMARY KEY(user_id,resource));
    CREATE TABLE IF NOT EXISTS work_epochs(inbox_id INTEGER PRIMARY KEY REFERENCES inbox(id),epoch INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS blocked_controls(inbox_id INTEGER PRIMARY KEY REFERENCES inbox(id));
    CREATE TABLE IF NOT EXISTS proposals(id TEXT PRIMARY KEY,requester TEXT NOT NULL REFERENCES users(id),operation TEXT NOT NULL,snapshot TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',created INTEGER NOT NULL DEFAULT(unixepoch()),expires INTEGER NOT NULL DEFAULT(unixepoch()+86400),reviewer TEXT REFERENCES users(id));
    CREATE TABLE IF NOT EXISTS control_outbox(id INTEGER PRIMARY KEY AUTOINCREMENT,proposal TEXT NOT NULL REFERENCES proposals(id),recipient TEXT NOT NULL REFERENCES users(id),kind TEXT NOT NULL CHECK(kind IN('review','result')),text TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',UNIQUE(proposal,recipient,kind));
    CREATE TABLE IF NOT EXISTS approval_labels(number INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,entity TEXT NOT NULL,UNIQUE(kind,entity));
    CREATE TABLE IF NOT EXISTS proposal_summaries(proposal TEXT PRIMARY KEY REFERENCES proposals(id),summary TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS approval_notifications(control_id INTEGER PRIMARY KEY REFERENCES control_outbox(id),stanza_id TEXT NOT NULL UNIQUE,identity TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS approval_quotes(inbox_id INTEGER PRIMARY KEY REFERENCES inbox(id),stanza_id TEXT NOT NULL);
    INSERT OR IGNORE INTO settings VALUES('approval_admin_revision','0');
    INSERT OR IGNORE INTO settings VALUES('authority_epoch','0');
    INSERT OR IGNORE INTO settings VALUES('history_floor','0');
    UPDATE control_outbox SET state='unknown' WHERE state='sending';")?;
    crate::coordination::migrate(db)?;
    crate::profiles::migrate(db)?;
    // Upgrade old notices once. Never replay uncertain sends.
    let mut q=db.prepare("SELECT id,operation FROM proposals WHERE id NOT IN(SELECT proposal FROM proposal_summaries)")?;
    let rows = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, op) in rows {
        let summary = change_summary(db, &serde_json::from_str(&op)?)?;
        db.execute(
            "INSERT INTO proposal_summaries VALUES(?,?)",
            params![id, summary],
        )?;
    }
    let mut q=db.prepare("SELECT o.id,s.summary,p.expires FROM control_outbox o JOIN proposals p ON p.id=o.proposal JOIN proposal_summaries s ON s.proposal=p.id WHERE o.kind='review' AND o.state IN('pending','sent') AND p.state='pending' AND p.expires>unixepoch() AND o.id NOT IN(SELECT control_id FROM approval_notifications)")?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, summary, expires) in rows {
        let expiry: String = db.query_row(
            "SELECT strftime('%Y-%m-%d %H:%M UTC',?,'unixepoch')",
            [expires],
            |r| r.get(0),
        )?;
        db.execute("UPDATE control_outbox SET text=?,state='pending' WHERE id=?",params![format!("{summary}\nReply to this message with *approve* or *reject*.\nExpires {expiry}."),id])?;
    }
    Ok(())
}
// Display only metadata: never expose private resource titles through the operator UI.
fn change_summary(db: &Connection, change: &Change) -> Result<String> {
    fn reference(db: &Connection, table: &str, id: &str, kind: &str) -> Result<String> {
        // Persistent aliases survive VACUUM, renames and unrelated inserts.
        db.execute(
            "INSERT OR IGNORE INTO approval_labels(kind,entity) VALUES(?,?)",
            params![table, id],
        )?;
        let n: i64 = db.query_row(
            "SELECT number FROM approval_labels WHERE kind=? AND entity=?",
            params![table, id],
            |r| r.get(0),
        )?;
        Ok(format!("{kind} #{n}"))
    }
    fn name(value: &str) -> String {
        value
            .chars()
            .filter(|c| !c.is_control() && !"*_~`".contains(*c))
            .take(100)
            .collect()
    }
    let group = |id: &str| -> Result<String> {
        let title: String =
            db.query_row("SELECT name FROM access_groups WHERE id=?", [id], |r| {
                r.get(0)
            })?;
        Ok(format!(
            "*{}* ({})",
            name(&title),
            reference(db, "access_groups", id, "group")?
        ))
    };
    let resource = |id: &str| reference(db, "resources", id, "resource");
    let person = |id: &str| reference(db, "users", id, "participant");
    let audience = |a: &Audience| -> Result<String> {
        match a {
            Audience::Everyone => Ok("everyone".into()),
            Audience::User { id } => person(id),
            Audience::Group { id } => group(id),
        }
    };
    let admin_target = |user: &str| -> Result<String> {
        let label:String=db.query_row("SELECT COALESCE(l.label,i.identity) FROM identities i LEFT JOIN participant_labels l ON l.identity=i.identity WHERE i.user_id=?",[user],|r|r.get(0))?;
        Ok(name(&label))
    };
    Ok(match change {
        Change::CreateCoordinationScope {requesters,recipients,resources} => format!("Allow {} to contact {}? Fresh requests and quoted replies may be shared within each pair without further approval. {} Private history stays excluded.",group(requesters)?,group(recipients)?,if *resources {"Include mutually readable resources and new uploads sent as conversation replies."}else{"Text only."}),
        Change::SetCoordinationResources {scope,enabled} => format!("{} resources for {}? {} Existing conversations close.",if *enabled {"Enable"}else{"Disable"},reference(db,"coordination_scopes",scope,"scope")?,if *enabled {"Allow mutually readable material and new uploads sent as conversation replies, without per-file approval."}else{"Return to text only."}),
        Change::SetCoordinationScope {scope,enabled} => format!("{} {}?",if *enabled {"Enable fresh text coordination through"} else {"Disable"},reference(db,"coordination_scopes",scope,"scope")?),
        Change::AddApprovalAdmin { user } => format!(
            "Add *{}* as approval admin (can approve permission changes)?",
            admin_target(user)?
        ),
        Change::RemoveApprovalAdmin { user } => {
            format!("Remove *{}* as approval admin?", admin_target(user)?)
        }
        Change::CreateGroup { name: title } => format!("Create group *{}*?", name(title)),
        Change::AddMember { group: g, user } => format!("Add {} to {}?", person(user)?, group(g)?),
        Change::RemoveMember { group: g, user } => {
            format!("Remove {} from {}?", person(user)?, group(g)?)
        }
        Change::SetGroupAdmin { group: g, enabled } => format!(
            "{} group-management rights for {}?",
            if *enabled { "Enable" } else { "Disable" },
            group(g)?
        ),
        Change::GrantRead {
            resource: r,
            audience: a,
        } => format!("Allow {} to read {}?", audience(a)?, resource(r)?),
        Change::RevokeRead {
            resource: r,
            audience: a,
        } => format!(
            "Remove read grant for {} on {}?",
            audience(a)?,
            resource(r)?
        ),
        Change::MoveResource {
            resource: r,
            parent,
        } => format!(
            "Move {} to {}? Inherited access may change.",
            resource(r)?,
            parent
                .as_deref()
                .map(resource)
                .transpose()?
                .unwrap_or_else(|| "owner's root folder".into())
        ),
    })
}
pub fn epoch(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT CAST(value AS INTEGER) FROM settings WHERE key='authority_epoch'",
        [],
        |r| r.get(0),
    )?)
}
pub fn reviewers(db: &Connection) -> Result<Vec<String>> {
    let value: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key='true_admins'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value
        .map(|v| serde_json::from_str(&v))
        .transpose()?
        .unwrap_or_default())
}
pub(crate) fn is_reviewer(db: &Connection, actor: &str) -> Result<bool> {
    Ok(reviewers(db)?.iter().any(|id| id == actor))
}
fn is_manager(db: &Connection, actor: &str) -> Result<bool> {
    Ok(is_reviewer(db, actor)? || db.query_row("SELECT EXISTS(SELECT 1 FROM memberships m JOIN access_groups g ON g.id=m.group_id WHERE m.user_id=? AND g.admin=1)",[actor],|r|r.get::<_,bool>(0))?)
}
fn known_user(db: &Connection, id: &str) -> Result<()> {
    ensure!(
        db.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?)", [id], |r| r
            .get::<_, bool>(0))?,
        "Unknown participant"
    );
    Ok(())
}
fn group(db: &Connection, id: &str) -> Result<(bool, i64)> {
    db.query_row(
        "SELECT admin,revision FROM access_groups WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .context("Group unavailable")
}
fn audience_exists(db: &Connection, audience: &Audience) -> Result<()> {
    match audience {
        Audience::User { id } => known_user(db, id),
        Audience::Group { id } => group(db, id).map(|_| ()),
        Audience::Everyone => Ok(()),
    }
}
fn ancestors(db: &Connection, resource: &str) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut current = Some(resource.to_owned());
    while let Some(id) = current {
        ensure!(
            ids.len() < 16 && !ids.contains(&id),
            "Invalid resource hierarchy"
        );
        current = db.query_row(
            "SELECT parent FROM resource_meta WHERE resource=?",
            [&id],
            |r| r.get::<_, Option<String>>(0),
        )?;
        ids.push(id);
    }
    Ok(ids)
}
fn descendants(db: &Connection, resource: &str) -> Result<Vec<String>> {
    let mut q=db.prepare("WITH RECURSIVE children(id) AS (SELECT ?1 UNION SELECT m.resource FROM resource_meta m JOIN children c ON m.parent=c.id) SELECT id FROM children LIMIT 101")?;
    let rows = q
        .query_map([resource], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(rows.len() <= 100, "Subtree limit exceeded");
    Ok(rows)
}
pub(crate) fn sources(db: &Connection, resource: &str) -> Result<Vec<String>> {
    let mut q = db.prepare("SELECT source FROM resource_sources WHERE resource=? LIMIT 4097")?;
    let rows = q
        .query_map([resource], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(rows.len() <= 4096, "Source limit exceeded");
    Ok(rows)
}
fn base_read(db: &Connection, user: &str, resource: &str) -> Result<bool> {
    let owner: Option<String> = db
        .query_row("SELECT owner FROM resources WHERE id=?", [resource], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(owner) = owner else { return Ok(false) };
    let path = ancestors(db, resource)?;
    if owner == user {
        return Ok(true);
    }
    if crate::profiles::directory_read(db, user, resource)? {
        return Ok(true);
    }
    for id in path {
        if db.query_row("SELECT EXISTS(SELECT 1 FROM read_grants g WHERE g.resource=?1 AND (g.kind='everyone' OR (g.kind='user' AND g.principal=?2) OR (g.kind='group' AND EXISTS(SELECT 1 FROM memberships m WHERE m.group_id=g.principal AND m.user_id=?2))))",params![id,user],|r|r.get::<_,bool>(0))? {return Ok(true)}
    }
    Ok(false)
}
pub fn can_read(db: &Connection, user: &str, resource: &str) -> Result<bool> {
    readable(
        db,
        &Audience::User { id: user.into() },
        resource,
        0,
        &mut HashMap::new(),
        &mut 4096,
    )
}
fn readable(
    db: &Connection,
    audience: &Audience,
    resource: &str,
    depth: usize,
    cache: &mut HashMap<(String, usize), bool>,
    budget: &mut usize,
) -> Result<bool> {
    if depth >= 32 || *budget == 0 {
        return Ok(false);
    }
    *budget -= 1;
    let key = (resource.to_owned(), depth);
    if let Some(value) = cache.get(&key) {
        return Ok(*value);
    }
    let (kind, id) = audience.key();
    let allowed = match audience {
        Audience::User { id } => base_read(db, id, resource)?,
        _ => {
            let mut allowed = false;
            for ancestor in ancestors(db, resource)? {
                allowed |= db.query_row("SELECT EXISTS(SELECT 1 FROM read_grants WHERE resource=?1 AND (kind='everyone' OR (kind=?2 AND principal=?3)))",params![ancestor,kind,id],|r|r.get::<_,bool>(0))?;
            }
            allowed
        }
    };
    if !allowed {
        cache.insert(key, false);
        return Ok(false);
    }
    let sources = sources(db, resource)?;
    for source in sources {
        if !readable(db, audience, &source, depth + 1, cache, budget)? {
            cache.insert(key, false);
            return Ok(false);
        }
    }
    cache.insert(key, true);
    Ok(true)
}
pub(crate) fn audience_reads(db: &Connection, audience: &Audience, resource: &str) -> Result<bool> {
    readable(db, audience, resource, 0, &mut HashMap::new(), &mut 4096)
}
pub(crate) fn validate(db: &Connection, actor: &str, change: &Change) -> Result<String> {
    known_user(db, actor)?;
    match change {
        Change::CreateCoordinationScope {
            requesters,
            recipients,
            resources: _,
        } => {
            ensure!(
                is_reviewer(db, actor)?,
                "Only approval admins can establish coordination scopes"
            );
            group(db, requesters)?;
            group(db, recipients)?;
            ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM coordination_scopes WHERE requesters=? AND recipients=?)",params![requesters,recipients],|r|r.get::<_,bool>(0))?,"Scope already exists");
            ensure!(
                db.query_row("SELECT COUNT(*) FROM coordination_scopes", [], |r| r
                    .get::<_, i64>(0))?
                    < 200,
                "Scope limit reached"
            );
            Ok("absent".into())
        }
        Change::SetCoordinationResources { scope, enabled } => {
            ensure!(
                is_reviewer(db, actor)?,
                "Only approval admins can change coordination scopes"
            );
            let (active, revision): (bool, i64) = db.query_row(
                "SELECT resources,revision FROM coordination_scopes WHERE id=?",
                [scope],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ensure!(
                active != *enabled,
                "Resource capability already has that status"
            );
            Ok(format!("{active}:{revision}"))
        }
        Change::SetCoordinationScope { scope, enabled } => {
            ensure!(
                is_reviewer(db, actor)?,
                "Only approval admins can change coordination scopes"
            );
            let (active, revision): (bool, i64) = db.query_row(
                "SELECT enabled,revision FROM coordination_scopes WHERE id=?",
                [scope],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ensure!(active != *enabled, "Scope already has that status");
            Ok(format!("{active}:{revision}"))
        }
        Change::AddApprovalAdmin { user } | Change::RemoveApprovalAdmin { user } => {
            // The web console principal changes approval admins directly (operator authority).
            ensure!(
                is_reviewer(db, actor)? || actor == WEB_ADMIN_REQUESTER,
                "Only current approval admins may change approval admins"
            );
            known_user(db, user)?;
            let mut q = db.prepare("SELECT identity FROM identities WHERE user_id=?")?;
            let identities = q
                .query_map([user], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                identities.len() == 1,
                "Choose a participant verified by a private WhatsApp message"
            );
            let mut admins = reviewers(db)?;
            admins.sort();
            let removing = matches!(change, Change::RemoveApprovalAdmin { .. });
            ensure!(
                admins.contains(user) == removing,
                "Approval admin already has that status"
            );
            ensure!(
                !removing || admins.len() > 1,
                "Cannot remove the last approval admin; add another first"
            );
            ensure!(
                removing || admins.len() < 1000,
                "Approval admin limit reached"
            );
            let generation = |id: &str| -> Result<i64> {
                Ok(db.query_row("SELECT COALESCE((SELECT generation FROM approval_admin_generations WHERE user_id=?),0)",[id],|r|r.get(0))?)
            };
            Ok(json!({"requester_generation":generation(actor)?,"target_generation":generation(user)?,"present":removing,"identity":identities[0]}).to_string())
        }
        Change::MoveResource { resource, parent } => {
            let owner: String = db
                .query_row("SELECT owner FROM resources WHERE id=?", [resource], |r| {
                    r.get(0)
                })
                .optional()?
                .context("Resource unavailable")?;
            ensure!(owner == actor, "Only owners may move resources");
            let old: Option<String> = db.query_row(
                "SELECT parent FROM resource_meta WHERE resource=?",
                [resource],
                |r| r.get(0),
            )?;
            ensure!(&old != parent, "Resource already in that folder");
            let children = descendants(db, resource)?;
            if let Some(parent) = parent {
                let (owner,kind):(String,String)=db.query_row("SELECT r.owner,m.kind FROM resources r JOIN resource_meta m ON m.resource=r.id WHERE r.id=?",[parent],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.context("Folder unavailable")?;
                ensure!(
                    owner == actor && kind == "folder",
                    "Destination must be your own folder"
                );
                let path = ancestors(db, parent)?;
                ensure!(!path.contains(resource), "Folder cycle rejected");
                let old_depth = ancestors(db, resource)?.len();
                for child in &children {
                    ensure!(
                        ancestors(db, child)?.len() - old_depth + 1 + path.len() <= 16,
                        "Folder nesting limit reached"
                    );
                }
                for ancestor in &path {
                    let mut q =
                        db.prepare("SELECT kind,principal FROM read_grants WHERE resource=?")?;
                    for grant in q
                        .query_map([ancestor], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                    {
                        let audience = match grant.0.as_str() {
                            "user" => Audience::User { id: grant.1 },
                            "group" => Audience::Group { id: grant.1 },
                            _ => Audience::Everyone,
                        };
                        for child in &children {
                            for source in sources(db, child)? {
                                ensure!(
                                    audience_reads(db, &audience, &source)?,
                                    "Destination audience cannot read a source"
                                );
                            }
                        }
                    }
                }
            }
            Ok(format!("{}:{}", epoch(db)?, serde_json::to_string(&old)?))
        }
        Change::CreateGroup { name } => {
            ensure!(
                is_manager(db, actor)?,
                "Group administration is not allowed"
            );
            ensure!(
                !name.trim().is_empty() && name.len() <= 80 && !name.chars().any(char::is_control),
                "Invalid group name"
            );
            ensure!(
                db.query_row("SELECT COUNT(*) FROM access_groups", [], |r| r
                    .get::<_, i64>(0))?
                    < 200,
                "Group limit reached"
            );
            ensure!(
                !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM access_groups WHERE name=? COLLATE NOCASE)",
                    [name],
                    |r| r.get::<_, bool>(0)
                )?,
                "Group already exists"
            );
            Ok("absent".into())
        }
        Change::AddMember { group: g, user } | Change::RemoveMember { group: g, user } => {
            known_user(db, user)?;
            let (admin, version) = group(db, g)?;
            ensure!(
                if admin {
                    is_reviewer(db, actor)?
                } else {
                    is_manager(db, actor)?
                },
                "Membership administration is not allowed"
            );
            let member: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM memberships WHERE group_id=? AND user_id=?)",
                params![g, user],
                |r| r.get(0),
            )?;
            ensure!(
                member == matches!(change, Change::RemoveMember { .. }),
                "Membership already has that state"
            );
            Ok(format!("{version}:{admin}:{member}"))
        }
        Change::SetGroupAdmin { group: g, enabled } => {
            ensure!(
                is_reviewer(db, actor)?,
                "Only sealed reviewers may delegate administration"
            );
            let (admin, version) = group(db, g)?;
            ensure!(admin != *enabled, "Capability already has that state");
            Ok(format!("{version}:{admin}"))
        }
        Change::GrantRead { resource, audience } | Change::RevokeRead { resource, audience } => {
            audience_exists(db, audience)?;
            let (owner, oid): (String, String) = db
                .query_row(
                    "SELECT owner,oid FROM resources WHERE id=?",
                    [resource],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .context("Resource unavailable")?;
            ensure!(
                owner == actor,
                "Only the resource owner may change its grants"
            );
            let (kind, id) = audience.key();
            let present:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM read_grants WHERE resource=? AND kind=? AND principal=?)",params![resource,kind,id],|r|r.get(0))?;
            ensure!(
                present == matches!(change, Change::RevokeRead { .. }),
                "Grant already has that state"
            );
            if matches!(change, Change::GrantRead { .. }) {
                for child in descendants(db, resource)? {
                    for source in sources(db, &child)? {
                        ensure!(
                            audience_reads(db, audience, &source)?,
                            "Share source resources with this audience first"
                        );
                    }
                }
            }

            let group_version = if let Audience::Group { id } = audience {
                group(db, id)?.1
            } else {
                0
            };
            let revision: i64 = db.query_row(
                "SELECT COALESCE((SELECT revision FROM resource_acl_versions WHERE resource=?),0)",
                [resource],
                |r| r.get(0),
            )?;
            Ok(format!(
                "{owner}:{oid}:{present}:{group_version}:{revision}:{}",
                epoch(db)?
            ))
        }
    }
}
pub(crate) fn apply(db: &Connection, id: &str, actor: &str, change: &Change) -> Result<()> {
    match change {
        Change::CreateCoordinationScope {
            requesters,
            recipients,
            resources,
        } => {
            db.execute(
                "INSERT INTO coordination_scopes(id,requesters,recipients,resources) VALUES(?,?,?,?)",
                params![id, requesters, recipients,resources],
            )?;
        }
        Change::SetCoordinationResources { scope, enabled } => {
            db.execute(
                "UPDATE coordination_scopes SET resources=?,revision=revision+1 WHERE id=?",
                params![enabled, scope],
            )?;
        }
        Change::SetCoordinationScope { scope, enabled } => {
            db.execute(
                "UPDATE coordination_scopes SET enabled=?,revision=revision+1 WHERE id=?",
                params![enabled, scope],
            )?;
        }
        Change::AddApprovalAdmin { user } | Change::RemoveApprovalAdmin { user } => {
            let mut admins = reviewers(db)?;
            if matches!(change, Change::AddApprovalAdmin { .. }) {
                admins.push(user.clone());
            } else {
                admins.retain(|id| id != user);
            }
            ensure!(!admins.is_empty(), "Cannot remove the last approval admin");
            db.execute(
                "UPDATE settings SET value=? WHERE key='true_admins'",
                [serde_json::to_string(&admins)?],
            )?;
            db.execute("UPDATE settings SET value=CAST(value AS INTEGER)+1 WHERE key='approval_admin_revision'",[])?;
            db.execute("INSERT INTO approval_admin_generations VALUES(?,1) ON CONFLICT(user_id) DO UPDATE SET generation=generation+1",[user])?;
        }
        Change::MoveResource { resource, parent } => {
            db.execute(
                "UPDATE resource_meta SET parent=? WHERE resource=?",
                params![parent, resource],
            )?;
        }
        Change::CreateGroup { name } => {
            db.execute(
                "INSERT INTO access_groups(id,name,owner) VALUES(?,?,?)",
                params![id, name, actor],
            )?;
        }
        Change::AddMember { group, user } => {
            db.execute("INSERT INTO memberships VALUES(?,?)", params![group, user])?;
            db.execute(
                "UPDATE access_groups SET revision=revision+1 WHERE id=?",
                [group],
            )?;
        }
        Change::RemoveMember { group, user } => {
            db.execute(
                "DELETE FROM memberships WHERE group_id=? AND user_id=?",
                params![group, user],
            )?;
            db.execute(
                "UPDATE access_groups SET revision=revision+1 WHERE id=?",
                [group],
            )?;
        }
        Change::SetGroupAdmin { group, enabled } => {
            db.execute(
                "UPDATE access_groups SET admin=?,revision=revision+1 WHERE id=?",
                params![enabled, group],
            )?;
        }
        Change::GrantRead { resource, audience } => {
            let (kind, id) = audience.key();
            db.execute(
                "INSERT INTO read_grants VALUES(?,?,?)",
                params![resource, kind, id],
            )?;
        }
        Change::RevokeRead { resource, audience } => {
            let (kind, id) = audience.key();
            db.execute(
                "DELETE FROM read_grants WHERE resource=? AND kind=? AND principal=?",
                params![resource, kind, id],
            )?;
        }
    }
    if let Change::GrantRead { resource, .. } | Change::RevokeRead { resource, .. } = change {
        db.execute("INSERT INTO resource_acl_versions VALUES(?,1) ON CONFLICT(resource) DO UPDATE SET revision=revision+1",[resource])?;
    }
    // Conservative context barrier: old model history/work cannot retain revoked authority.
    db.execute(
        "UPDATE settings SET value=CAST(value AS INTEGER)+1 WHERE key='authority_epoch'",
        [],
    )?;
    db.execute("UPDATE settings SET value=(SELECT COALESCE(MAX(id),0) FROM messages) WHERE key='history_floor'",[])?;
    db.execute("DELETE FROM context_sources", [])?;
    db.execute(
        "UPDATE outbox SET state='cancelled' WHERE state='pending'",
        [],
    )?;
    Ok(())
}
/// Called in the same transaction as an admin-set update. Do not replay uncertain sends.
fn reconcile_admin_requests(db: &Connection) -> Result<()> {
    let admins = reviewers(db)?;
    let mut q = db.prepare(
        "SELECT id,recipient FROM control_outbox WHERE kind='review' AND state='pending'",
    )?;
    let queued = q
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, recipient) in queued {
        if !admins.contains(&recipient) {
            db.execute(
                "UPDATE control_outbox SET state='cancelled' WHERE id=?",
                [id],
            )?;
        }
    }
    let mut q=db.prepare("SELECT p.id,p.requester,p.operation,p.snapshot,s.summary,strftime('%Y-%m-%d %H:%M UTC',p.expires,'unixepoch') FROM proposals p JOIN proposal_summaries s ON s.proposal=p.id WHERE p.state='pending' AND p.expires>unixepoch() LIMIT 50")?;
    let pending = q
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, actor, operation, snapshot, summary, expiry) in pending {
        let change: Change = serde_json::from_str(&operation)?;
        if !validate(db, &actor, &change).is_ok_and(|value| value == snapshot) {
            db.execute("UPDATE proposals SET state='stale' WHERE id=?", [&id])?;
            db.execute("UPDATE control_outbox SET state='cancelled' WHERE proposal=? AND kind='review' AND state='pending'",[&id])?;
            if actor != WEB_ADMIN_REQUESTER {
                db.execute("INSERT OR IGNORE INTO control_outbox(proposal,recipient,kind,text) VALUES(?,?,'result',?)",params![id,actor,format!("{}: stale after admin changes. Request again if still needed.",summary.trim_end_matches('?'))])?;
            }
            db.execute(
                "INSERT INTO audit(event,subject) VALUES('permission_invalidated',?)",
                [json!({"id":id,"reason":"approval_admins_changed"}).to_string()],
            )?;
            continue;
        }
        let text = format!(
            "{summary}\nReply to this message with *approve* or *reject*.\nExpires {expiry}."
        );
        for admin in &admins {
            db.execute("INSERT INTO control_outbox(proposal,recipient,kind,text) VALUES(?,?,'review',?) ON CONFLICT(proposal,recipient,kind) DO UPDATE SET state='pending',text=excluded.text WHERE control_outbox.state='cancelled'",params![id,admin,text])?;
        }
    }
    Ok(())
}
pub fn control_verb(text: &str) -> bool {
    text.split_whitespace().next().is_some_and(|s| {
        ["approve", "approving", "reject", "rejecting"]
            .iter()
            .any(|v| s.eq_ignore_ascii_case(v))
    })
}
fn command(text: &str) -> Result<(bool, String)> {
    let words: Vec<_> = text.split_whitespace().collect();
    ensure!(
        words.len() == 2 && control_verb(text),
        "Use WhatsApp Reply on the approval request, then send approve or reject"
    );
    Ok((
        words[0].eq_ignore_ascii_case("approve") || words[0].eq_ignore_ascii_case("approving"),
        Uuid::parse_str(words[1])
            .context("Use WhatsApp Reply on the approval request, then send approve or reject")?
            .to_string(),
    ))
}
impl Store {
    pub fn check_work(&self, w: &Work) -> Result<()> {
        let db = self.db.lock().unwrap();
        let expected: i64 = db.query_row(
            "SELECT epoch FROM work_epochs WHERE inbox_id=?",
            [w.id],
            |r| r.get(0),
        )?;
        ensure!(
            epoch(&db)? == expected,
            "Permissions changed; discard this model work"
        );
        let mut q = db.prepare("SELECT resource FROM profile_reads WHERE inbox=?")?;
        for id in q.query_map([w.id], |r| r.get::<_, String>(0))? {
            ensure!(
                can_read(&db, &w.user, &id?)?,
                "Profile changed or lost access; discard this model work"
            );
        }
        Ok(())
    }
    pub fn permission_directory(&self, actor: &str) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let manager = is_manager(&db, actor)?;
        let approval_admin = is_reviewer(&db, actor)?;
        let mut q=db.prepare("SELECT g.id,g.name,g.admin FROM access_groups g WHERE ?1 OR EXISTS(SELECT 1 FROM memberships m WHERE m.group_id=g.id AND m.user_id=?2) ORDER BY g.name LIMIT 200")?;
        let groups=q.query_map(params![manager,actor],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"admin":r.get::<_,bool>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut people = Vec::new();
        if manager {
            let mut q = db.prepare("SELECT i.user_id,i.identity,COALESCE(l.label,i.identity) FROM identities i LEFT JOIN participant_labels l ON l.identity=i.identity LIMIT 1000")?;
            people = q
                .query_map([], |r| {
                    Ok(json!({"id":r.get::<_,String>(0)?,"identity":r.get::<_,String>(1)?,"label":r.get::<_,String>(2)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
        }
        Ok(
            json!({"self":actor,"can_manage_groups":manager,"can_manage_approval_admins":approval_admin,"approval_available":!reviewers(&db)?.is_empty(),"approval_admins_visible":approval_admin,"approval_admins":if approval_admin {reviewers(&db)?}else{Vec::<String>::new()},"groups":groups,"participants":people}),
        )
    }
    pub fn add_web_admin(&self, user: String) -> Result<Value> {
        self.change_web_admin(Change::AddApprovalAdmin { user })
    }
    pub fn remove_web_admin(&self, user: String) -> Result<Value> {
        self.change_web_admin(Change::RemoveApprovalAdmin { user })
    }
    /// The web console acts as operator: admin changes apply at once, with the same identity,
    /// last-admin, barrier and notice handling as an approved WhatsApp request.
    fn change_web_admin(&self, change: Change) -> Result<Value> {
        let (user, source, notice) = match &change {
            Change::AddApprovalAdmin { user } => (user.clone(), "Added from Carbon web UI.", "You are now a Carbon approval admin. To decide a request, use WhatsApp Reply on its approval notice and send approve or reject."),
            Change::RemoveApprovalAdmin { user } => (user.clone(), "Removed from Carbon web UI.", "You are no longer a Carbon approval admin."),
            _ => bail!("Unsupported web admin change"),
        };
        let _authority = self.authority.lock().unwrap();
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        ensure!(
            !reviewers(&tx)?.is_empty(),
            "Select initial reviewers before changing approval admins"
        );
        tx.execute(
            "INSERT OR IGNORE INTO users(id) VALUES(?)",
            [WEB_ADMIN_REQUESTER],
        )?;
        let snapshot = validate(&tx, WEB_ADMIN_REQUESTER, &change)?;
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO proposals(id,requester,operation,snapshot,state,reviewer) VALUES(?,?,?,?,'approved',?)",
            params![id, WEB_ADMIN_REQUESTER, serde_json::to_string(&change)?, snapshot, WEB_ADMIN_REQUESTER],
        )?;
        let summary = change_summary(&tx, &change)?;
        tx.execute(
            "INSERT INTO proposal_summaries VALUES(?,?)",
            params![id, format!("{}\n{source}", summary.trim_end_matches('?'))],
        )?;
        apply(&tx, &id, WEB_ADMIN_REQUESTER, &change)?;
        tx.execute(
            "INSERT INTO audit(event,subject) VALUES('permission_decided',?)",
            [json!({"id":id,"reviewer":WEB_ADMIN_REQUESTER,"state":"approved","operation":change}).to_string()],
        )?;
        reconcile_admin_requests(&tx)?;
        tx.execute(
            "INSERT OR IGNORE INTO control_outbox(proposal,recipient,kind,text) VALUES(?,?,'result',?)",
            params![id, user, notice],
        )?;
        tx.commit()?;
        Ok(json!({"request_id":id,"state":"approved"}))
    }
    pub fn propose(&self, actor: &str, change: Change) -> Result<Value> {
        ensure!(
            actor != WEB_ADMIN_REQUESTER,
            "Web requests require the authenticated web endpoint"
        );
        self.propose_internal(actor, change)
    }
    fn propose_internal(&self, actor: &str, change: Change) -> Result<Value> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let reviewers = reviewers(&tx)?;
        ensure!(
            !reviewers.is_empty(),
            "Select initial reviewers before requesting permission changes"
        );
        if actor == WEB_ADMIN_REQUESTER {
            tx.execute(
                "INSERT OR IGNORE INTO users(id) VALUES(?)",
                [WEB_ADMIN_REQUESTER],
            )?;
        }
        let snapshot = validate(&tx, actor, &change)?;
        let operation = serde_json::to_string(&change)?;
        if let Some(id)=tx.query_row("SELECT id FROM proposals WHERE requester=? AND operation=? AND state='pending' AND expires>unixepoch()",params![actor,operation],|r|r.get::<_,String>(0)).optional()? {return Ok(json!({"request_id":id,"state":"pending"}));}
        ensure!(
            tx.query_row(
                "SELECT COUNT(*) FROM proposals WHERE requester=? AND created>unixepoch()-86400",
                [actor],
                |r| r.get::<_, i64>(0)
            )? < 20,
            "Daily proposal limit reached"
        );
        ensure!(
            tx.query_row(
                "SELECT COUNT(*) FROM proposals WHERE state='pending' AND expires>unixepoch()",
                [],
                |r| r.get::<_, i64>(0)
            )? < 50,
            "Pending proposal limit reached"
        );
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO proposals(id,requester,operation,snapshot) VALUES(?,?,?,?)",
            params![id, actor, operation, snapshot],
        )?;
        let summary = change_summary(&tx, &change)?;
        let summary = if actor == WEB_ADMIN_REQUESTER {
            format!("{summary}\nRequested from Carbon web UI.")
        } else {
            summary
        };
        tx.execute(
            "INSERT INTO proposal_summaries VALUES(?,?)",
            params![id, summary],
        )?;
        let body = format!(
            "{}\nReply to this message with *approve* or *reject*.\nExpires in 24h.",
            summary
        );
        for reviewer in reviewers.iter() {
            tx.execute(
                "INSERT INTO control_outbox(proposal,recipient,kind,text) VALUES(?,?,'review',?)",
                params![id, reviewer, body],
            )?;
        }
        tx.execute(
            "INSERT INTO audit(event,subject) VALUES('permission_proposed',?)",
            [json!({"id":id,"requester":actor,"operation":change}).to_string()],
        )?;
        tx.commit()?;
        Ok(
            json!({"request_id":id,"state":"pending","expires_in":86400,"group_id":if matches!(change,Change::CreateGroup{..}) {Some(id.clone())}else{None}}),
        )
    }
    pub fn pending_reviews(&self, actor: &str) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let reviewer = is_reviewer(&db, actor)?;
        let mut q=db.prepare("SELECT id,requester,operation,expires FROM proposals WHERE state='pending' AND expires>unixepoch() AND (requester=?1 OR ?2) ORDER BY created,id LIMIT 50")?;
        let rows = q
            .query_map(params![actor, reviewer], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let items=rows.into_iter().map(|(id,requester,operation,expires)|Ok(json!({"request_id":id,"requester":requester,"operation":serde_json::from_str::<Value>(&operation)?,"expires":expires,"can_review":reviewer,"approval_instruction":"Use WhatsApp Reply on the approval message and send approve or reject. Do not display internal request IDs."}))).collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"requests":items,"notice":"Pending requests only. Review the exact structured change. Approval requires the human reviewer to send the request-bound command; the model cannot approve."}),
        )
    }
    pub fn proposal_status(&self, actor: &str, id: &str) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let (requester, operation, state, expires): (String, String, String, i64) = db
            .query_row(
                "SELECT requester,operation,state,expires FROM proposals WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .context("Request unavailable")?;
        ensure!(
            requester == actor || is_reviewer(&db, actor)?,
            "Request unavailable"
        );
        Ok(
            json!({"request_id":id,"requester":requester,"operation":serde_json::from_str::<Value>(&operation)?,"state":state,"expires":expires}),
        )
    }
    /// Reserved approvals are selected by a recorded notice or legacy proposal ID,
    /// not merely by an ordinary conversation containing the word approve/reject.
    pub(crate) fn admin_control_bound(&self, w: &Work) -> Result<bool> {
        if !control_verb(&w.text) {
            return Ok(false);
        }
        let db = self.db.lock().unwrap();
        if db.query_row("SELECT EXISTS(SELECT 1 FROM approval_quotes q JOIN approval_notifications n ON n.stanza_id=q.stanza_id WHERE q.inbox_id=?)",[w.id],|r|r.get::<_,bool>(0))? {return Ok(true)}
        if let Ok((_, id)) = command(&w.text) {
            return Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM proposals WHERE id=?)",
                [id],
                |r| r.get::<_, bool>(0),
            )?);
        }
        Ok(false)
    }
    pub fn handle_control(&self, w: &Work) -> Option<String> {
        if !control_verb(&w.text) {
            return None;
        }
        let result = self.decide(w);
        Some(match result {
            Ok(text) => text,
            Err(e) => format!("Carbon: {e}"),
        })
    }
    fn decide(&self, w: &Work) -> Result<String> {
        let _authority = self.authority.lock().unwrap();
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM blocked_controls WHERE inbox_id=?)",
                [w.id],
                |r| r.get::<_, bool>(0)
            )?,
            "Forwarded approval commands are not accepted"
        );
        ensure!(
            is_reviewer(&tx, &w.user)?,
            "Only a sealed reviewer can decide requests"
        );
        let (approve, id) = if w.text.split_whitespace().count() == 1 {
            let id: String = tx.query_row("SELECT o.proposal FROM approval_quotes q JOIN approval_notifications n ON n.stanza_id=q.stanza_id JOIN control_outbox o ON o.id=n.control_id WHERE q.inbox_id=? AND o.recipient=? AND n.identity=? AND o.kind='review'",params![w.id,w.user,w.recipient],|r|r.get(0)).optional()?.context("Use WhatsApp Reply on the approval request, then send approve or reject; an unbound approve cannot authorize changes")?;
            (
                matches!(
                    w.text.trim().to_ascii_lowercase().as_str(),
                    "approve" | "approving"
                ),
                id,
            )
        } else {
            // Compatibility for approval requests delivered before reply binding existed.
            command(&w.text)?
        };
        let (requester,operation,snapshot,state,expired):(String,String,String,String,bool)=tx.query_row("SELECT requester,operation,snapshot,state,expires<=unixepoch() FROM proposals WHERE id=?",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.context("Request unavailable")?;
        ensure!(state == "pending", "Request already decided");
        let change: Change = serde_json::from_str(&operation)?;
        let next = if expired {
            "expired"
        } else if !approve {
            "rejected"
        } else if validate(&tx, &requester, &change).is_ok_and(|s| s == snapshot) {
            apply(&tx, &id, &requester, &change)?;
            "approved"
        } else {
            "stale"
        };
        tx.execute(
            "UPDATE proposals SET state=?,reviewer=? WHERE id=? AND state='pending'",
            params![next, w.user, id],
        )?;
        tx.execute("UPDATE control_outbox SET state='cancelled' WHERE proposal=? AND kind='review' AND state='pending'",[&id])?;
        let body = format!(
            "{}: {next}.",
            tx.query_row(
                "SELECT summary FROM proposal_summaries WHERE proposal=?",
                [&id],
                |r| r.get::<_, String>(0)
            )?
            .trim_end_matches('?')
        );
        if requester != WEB_ADMIN_REQUESTER {
            tx.execute(
                "INSERT INTO control_outbox(proposal,recipient,kind,text) VALUES(?,?,'result',?)",
                params![id, requester, body],
            )?;
        }
        tx.execute(
            "INSERT INTO audit(event,subject) VALUES('permission_decided',?)",
            [json!({"id":id,"reviewer":w.user,"state":next,"operation":change}).to_string()],
        )?;
        if next == "approved"
            && matches!(
                change,
                Change::AddApprovalAdmin { .. } | Change::RemoveApprovalAdmin { .. }
            )
        {
            reconcile_admin_requests(&tx)?;
            let (target,notice)=match &change {
                Change::AddApprovalAdmin{user} => (user,"You are now a Carbon approval admin. To decide a request, use WhatsApp Reply on its approval notice and send approve or reject."),
                Change::RemoveApprovalAdmin{user} => (user,"You are no longer a Carbon approval admin."),
                _=>unreachable!(),
            };
            tx.execute("INSERT OR IGNORE INTO control_outbox(proposal,recipient,kind,text) VALUES(?,?,'result',?)",params![id,target,notice])?;
        }
        // This canonical response is generated after applying the barrier, not by the model.
        tx.execute(
            "UPDATE work_epochs SET epoch=? WHERE inbox_id=?",
            params![epoch(&tx)?, w.id],
        )?;
        tx.commit()?;
        Ok(body)
    }
    pub fn claim_control(&self) -> Result<Option<(i64, String, String)>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE proposals SET state='expired' WHERE state='pending' AND expires<=unixepoch()",
            [],
        )?;
        loop {
            let row:Option<(i64,String,String,String,String)>=tx.query_row("SELECT id,proposal,recipient,kind,text FROM control_outbox WHERE state='pending' ORDER BY id LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let Some((id, proposal, user, kind, text)) = row else {
                tx.commit()?;
                return Ok(None);
            };
            let valid=kind=="result" || (is_reviewer(&tx,&user)? && tx.query_row("SELECT EXISTS(SELECT 1 FROM proposals WHERE id=? AND state='pending' AND expires>unixepoch())",params![proposal],|r|r.get::<_,bool>(0))?);
            let identities: Vec<String> = {
                let mut q = tx.prepare("SELECT identity FROM identities WHERE user_id=?")?;
                let rows = q
                    .query_map([&user], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            if !valid || identities.len() != 1 {
                tx.execute(
                    "UPDATE control_outbox SET state='cancelled' WHERE id=?",
                    [id],
                )?;
                continue;
            }
            tx.execute("INSERT OR IGNORE INTO approval_notifications(control_id,stanza_id,identity) VALUES(?,?,?)",params![id,Uuid::new_v4().simple().to_string().to_uppercase(),identities[0]])?;
            tx.execute("UPDATE control_outbox SET state='sending' WHERE id=?", [id])?;
            tx.commit()?;
            return Ok(Some((id, identities[0].clone(), text)));
        }
    }
    pub fn control_stanza(&self, id: i64) -> Result<String> {
        Ok(self.db.lock().unwrap().query_row(
            "SELECT stanza_id FROM approval_notifications WHERE control_id=?",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn approval_requests(&self) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT s.summary,CASE WHEN p.state='pending' AND p.expires<=unixepoch() THEN 'expired' ELSE p.state END,p.created,p.expires,(SELECT identity FROM identities WHERE user_id=p.requester LIMIT 1) FROM proposals p JOIN proposal_summaries s ON s.proposal=p.id ORDER BY (p.state='pending' AND p.expires>unixepoch()) DESC,p.created DESC,p.rowid DESC LIMIT 100")?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let requests=rows.into_iter().map(|(op,state,created,expires,identity)|Ok(json!({"summary":op,"state":state,"created":created,"expires":expires,"identity":identity}))).collect::<Result<Vec<_>>>()?;
        Ok(json!({"requests":requests}))
    }
    pub fn control_sent(&self, id: i64, success: bool) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE control_outbox SET state=? WHERE id=?",
            params![if success { "sent" } else { "unknown" }, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_change_schema_matches_strict_parser_for_every_variant() {
        fn samples(schema: &Value) -> Vec<Value> {
            if let Some(variants) = schema.get("anyOf") {
                return variants
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(samples)
                    .collect();
            }
            if let Some(values) = schema.get("enum") {
                return values.as_array().unwrap().clone();
            }
            if schema["type"] == "object" {
                assert_eq!(schema["additionalProperties"], false);
                let properties = schema["properties"].as_object().unwrap();
                let required = schema["required"].as_array().unwrap();
                assert_eq!(required.len(), properties.len());
                let mut objects = vec![serde_json::Map::new()];
                for (key, field) in properties {
                    assert!(required.contains(&json!(key)));
                    objects = objects
                        .into_iter()
                        .flat_map(|object| {
                            samples(field).into_iter().map(move |sample| {
                                let mut result = object.clone();
                                result.insert(key.clone(), sample);
                                result
                            })
                        })
                        .collect();
                }
                return objects.into_iter().map(Value::Object).collect();
            }
            if schema["type"] == "boolean" {
                return vec![json!(false), json!(true)];
            }
            if schema["type"].is_array() {
                return vec![Value::Null, json!("synthetic-id")];
            }
            assert_eq!(schema["type"], "string");
            vec![json!("synthetic-id")]
        }
        let schema = change_parameters();
        let mut seen = std::collections::BTreeSet::new();
        for sample in samples(&schema) {
            let change = sample["change"].clone();
            let parsed: Change = serde_json::from_value(change.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), change);
            seen.insert(change["op"].as_str().unwrap().to_owned());
            let mut extra = change;
            extra
                .as_object_mut()
                .unwrap()
                .insert("actor".into(), json!("forged"));
            assert!(serde_json::from_value::<Change>(extra).is_err());
        }
        assert_eq!(seen.len(), 12);
        // The observed superset grant must still be rejected by the authority parser.
        assert!(serde_json::from_value::<Change>(json!({"op":"grant_read","resource":"source","audience":{"kind":"group","id":"readers"},"enabled":false})).is_err());
    }

    fn message(s: &Store, identity: &str, text: &str) -> Work {
        s.ingest(identity, &Uuid::new_v4().to_string(), text)
            .unwrap();
        s.next().unwrap().unwrap()
    }
    fn setup() -> (tempfile::TempDir, Store, Work, Work, Work) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let a = message(&s, "a@test", "hi");
        let b = message(&s, "b@test", "hi");
        let c = message(&s, "c@test", "hi");
        s.bootstrap_admins(&[a.user.clone(), b.user.clone()])
            .unwrap();
        (d, s, a, b, c)
    }
    #[test]
    fn web_admin_additions_apply_immediately_to_verified_participants() {
        let (_d, s, _a, _b, c) = setup();
        assert!(s.add_web_admin("invented".into()).is_err());
        assert!(s
            .propose(
                WEB_ADMIN_REQUESTER,
                Change::AddApprovalAdmin {
                    user: c.user.clone()
                }
            )
            .is_err());
        assert!(s
            .propose(
                &c.user,
                Change::CreateGroup {
                    name: "test".into()
                }
            )
            .is_err());
        let directory = s.permission_directory(&c.user).unwrap();
        assert_eq!(directory["approval_available"], true);
        assert_eq!(directory["approval_admins_visible"], false);
        assert_eq!(directory["approval_admins"], json!([]));
        assert_eq!(
            s.add_web_admin(c.user.clone()).unwrap()["state"],
            "approved"
        );
        assert_eq!(
            s.permission_directory(&c.user).unwrap()["can_manage_approval_admins"],
            true
        );
        assert_eq!(reviewers(&s.db.lock().unwrap()).unwrap().len(), 3);
        let notice: String =
            s.db.lock()
                .unwrap()
                .query_row(
                    "SELECT text FROM control_outbox WHERE recipient=? AND kind='result'",
                    [&c.user],
                    |r| r.get(0),
                )
                .unwrap();
        assert!(notice.starts_with("You are now a Carbon approval admin."));
        assert!(s.add_web_admin(c.user).is_err());
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM control_outbox WHERE recipient=?",
                    [WEB_ADMIN_REQUESTER],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    #[test]
    fn web_admin_requests_cannot_bootstrap_or_grant_other_authority() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let user = message(&s, "user@test", "hello");
        assert_eq!(
            s.permission_directory(&user.user).unwrap()["approval_available"],
            false
        );
        assert!(s.add_web_admin(user.user.clone()).is_err());
        s.bootstrap_admins(&[user.user]).unwrap();
        assert!(s
            .propose_internal(
                WEB_ADMIN_REQUESTER,
                Change::CreateGroup {
                    name: "test".into()
                }
            )
            .is_err());
        assert!(s
            .propose_internal(
                WEB_ADMIN_REQUESTER,
                Change::RemoveApprovalAdmin {
                    user: "invented".into()
                }
            )
            .is_err());
    }
    #[test]
    fn web_admin_removal_applies_immediately_and_keeps_the_last_admin() {
        let (_d, s, a, b, c) = setup();
        let pending = propose(
            &s,
            &a.user,
            Change::AddApprovalAdmin {
                user: c.user.clone(),
            },
        );
        assert!(s.remove_web_admin(c.user.clone()).is_err());
        assert!(s.remove_web_admin("invented".into()).is_err());
        assert_eq!(
            s.remove_web_admin(b.user.clone()).unwrap()["state"],
            "approved"
        );
        assert_eq!(
            reviewers(&s.db.lock().unwrap()).unwrap(),
            std::slice::from_ref(&a.user)
        );
        let db = s.db.lock().unwrap();
        // The removed admin gets a notice and loses queued reviews; unrelated requests stay live.
        let notice: String = db
            .query_row(
                "SELECT text FROM control_outbox WHERE recipient=? AND kind='result'",
                [&b.user],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(notice, "You are no longer a Carbon approval admin.");
        let live: i64 = db.query_row("SELECT COUNT(*) FROM control_outbox WHERE recipient=? AND kind='review' AND state='pending'", [&b.user], |r| r.get(0)).unwrap();
        assert_eq!(live, 0);
        let state: String = db
            .query_row("SELECT state FROM proposals WHERE id=?", [&pending], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "pending");
        drop(db);
        assert!(s.remove_web_admin(a.user).is_err());
    }
    fn propose(s: &Store, actor: &str, change: Change) -> String {
        s.propose(actor, change).unwrap()["request_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn approve(s: &Store, identity: &str, id: &str) -> String {
        s.handle_control(&message(s, identity, &format!("ApPrOvInG {id}")))
            .unwrap()
    }
    #[test]
    fn independent_admin_additions_do_not_require_repeated_proposals() {
        let (_d, s, a, _, c) = setup();
        let d = message(&s, "fourth@test", "hello");
        let first = propose(
            &s,
            &a.user,
            Change::AddApprovalAdmin {
                user: c.user.clone(),
            },
        );
        let second = propose(
            &s,
            &a.user,
            Change::AddApprovalAdmin {
                user: d.user.clone(),
            },
        );
        assert!(approve(&s, &a.recipient, &first).contains(": approved."));
        assert_eq!(
            s.proposal_status(&a.user, &second).unwrap()["state"],
            "pending"
        );
        assert!(approve(&s, &a.recipient, &second).contains(": approved."));
        assert!(is_reviewer(&s.db.lock().unwrap(), &c.user).unwrap());
        assert!(is_reviewer(&s.db.lock().unwrap(), &d.user).unwrap());
    }
    #[test]
    fn approval_admin_handoff_is_explicit_verified_and_keeps_one_admin() {
        let (_d, s, a, b, c) = setup();
        assert!(s
            .propose(
                &c.user,
                Change::AddApprovalAdmin {
                    user: c.user.clone()
                }
            )
            .is_err());
        assert!(s
            .propose(
                &a.user,
                Change::AddApprovalAdmin {
                    user: "unknown".into()
                }
            )
            .is_err());
        let remove = propose(
            &s,
            &a.user,
            Change::RemoveApprovalAdmin {
                user: b.user.clone(),
            },
        );
        assert!(approve(&s, &a.recipient, &remove).contains(": approved."));
        assert!(s
            .propose(
                &a.user,
                Change::RemoveApprovalAdmin {
                    user: a.user.clone()
                }
            )
            .unwrap_err()
            .to_string()
            .contains("last approval admin"));
        s.cache_identity_label(&b.recipient, "+15555550101 (synthetic@lid)")
            .unwrap();
        let secret = s.save_note(&a.user, "Private", "PRIVATE_CANARY").unwrap();
        let add = propose(
            &s,
            &a.user,
            Change::AddApprovalAdmin {
                user: b.user.clone(),
            },
        );
        assert!(!is_reviewer(&s.db.lock().unwrap(), &b.user).unwrap());
        assert!(approve(&s, &b.recipient, &add).contains("sealed reviewer"));
        assert!(s
            .approval_requests()
            .unwrap()
            .to_string()
            .contains("+15555550101"));
        assert!(approve(&s, &a.recipient, &add).contains(": approved."));
        assert!(is_reviewer(&s.db.lock().unwrap(), &b.user).unwrap());
        assert!(s.read_note(&b.user, &secret).is_err());
        assert_eq!(
            s.permission_directory(&b.user).unwrap()["can_manage_approval_admins"],
            true
        );
        let remove = propose(
            &s,
            &b.user,
            Change::RemoveApprovalAdmin {
                user: a.user.clone(),
            },
        );
        assert!(approve(&s, &b.recipient, &remove).contains(": approved."));
        assert_eq!(
            reviewers(&s.db.lock().unwrap()).unwrap(),
            vec![b.user.clone()]
        );
        assert!(s
            .propose(
                &b.user,
                Change::RemoveApprovalAdmin {
                    user: b.user.clone()
                }
            )
            .is_err());
        assert!(approve(&s, &a.recipient, &remove).contains("sealed reviewer"));
        assert!(s.bootstrap_admins(&[a.user]).is_err());
    }
    #[test]
    fn group_admin_cannot_promote_approval_admins_or_use_display_labels_as_authority() {
        let (_d, s, a, _, c) = setup();
        let g = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Managers".into(),
            },
        );
        approve(&s, &a.recipient, &g);
        let admin = propose(
            &s,
            &a.user,
            Change::SetGroupAdmin {
                group: g.clone(),
                enabled: true,
            },
        );
        approve(&s, &a.recipient, &admin);
        let join = propose(
            &s,
            &a.user,
            Change::AddMember {
                group: g,
                user: c.user.clone(),
            },
        );
        approve(&s, &a.recipient, &join);
        s.cache_identity_label(&c.recipient, &a.recipient).unwrap();
        let dir = s.permission_directory(&c.user).unwrap();
        assert_eq!(dir["can_manage_groups"], true);
        assert_eq!(dir["can_manage_approval_admins"], false);
        assert!(dir["approval_admins"].as_array().unwrap().is_empty());
        assert!(s
            .propose(
                &c.user,
                Change::AddApprovalAdmin {
                    user: c.user.clone()
                }
            )
            .is_err());
        assert!(s
            .propose(&c.user, Change::RemoveApprovalAdmin { user: a.user })
            .is_err());
        assert!(!is_reviewer(&s.db.lock().unwrap(), &c.user).unwrap());
    }
    #[test]
    fn new_admin_receives_live_pending_notices_and_removed_admin_quotes_fail() {
        let (_d, s, a, b, c) = setup();
        let pending = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Pending team".into(),
            },
        );
        let (notice, recipient, _) = s.claim_control().unwrap().unwrap();
        let stanza = s.control_stanza(notice).unwrap();
        s.control_sent(notice, true).unwrap();
        assert_eq!(recipient, a.recipient);
        let add = propose(
            &s,
            &b.user,
            Change::AddApprovalAdmin {
                user: c.user.clone(),
            },
        );
        approve(&s, &b.recipient, &add);
        assert_eq!(s.db.lock().unwrap().query_row("SELECT state FROM control_outbox WHERE proposal=? AND recipient=? AND kind='review'",params![pending,c.user],|r|r.get::<_,String>(0)).unwrap(),"pending");
        let remove = propose(
            &s,
            &b.user,
            Change::RemoveApprovalAdmin {
                user: a.user.clone(),
            },
        );
        approve(&s, &b.recipient, &remove);
        assert_eq!(
            s.proposal_status(&b.user, &pending).unwrap()["state"],
            "stale"
        ); // requester lost group authority
        s.ingest_reply(
            &a.recipient,
            "old-admin-quote",
            "approve",
            true,
            Some(&stanza),
        )
        .unwrap();
        assert!(s
            .handle_control(&s.next_control().unwrap().unwrap())
            .unwrap()
            .contains("sealed reviewer"));
        assert!(!s.permission_directory(&c.user).unwrap()["groups"]
            .to_string()
            .contains("Pending team"));
    }
    #[test]
    fn uncertain_notifications_are_not_replayed_when_an_admin_is_readded() {
        let (_d, s, a, b, _) = setup();
        let pending = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Still pending".into(),
            },
        );
        let (first, _, _) = s.claim_control().unwrap().unwrap();
        s.control_sent(first, true).unwrap();
        let (second, recipient, _) = s.claim_control().unwrap().unwrap();
        assert_eq!(recipient, b.recipient);
        s.control_sent(second, false).unwrap();
        let remove = propose(
            &s,
            &a.user,
            Change::RemoveApprovalAdmin {
                user: b.user.clone(),
            },
        );
        approve(&s, &a.recipient, &remove);
        let add = propose(
            &s,
            &a.user,
            Change::AddApprovalAdmin {
                user: b.user.clone(),
            },
        );
        approve(&s, &a.recipient, &add);
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row(
                    "SELECT state FROM control_outbox WHERE id=?",
                    [second],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "unknown"
        );
        assert_eq!(
            s.proposal_status(&a.user, &pending).unwrap()["state"],
            "pending"
        );
    }
    #[test]
    fn concurrent_admin_removals_cannot_empty_the_admin_set() {
        let (_d, s, a, b, _) = setup();
        let s = std::sync::Arc::new(s);
        let first = propose(
            &s,
            &a.user,
            Change::RemoveApprovalAdmin {
                user: b.user.clone(),
            },
        );
        let second = propose(
            &s,
            &b.user,
            Change::RemoveApprovalAdmin {
                user: a.user.clone(),
            },
        );
        let wa = message(&s, &a.recipient, &format!("approve {first}"));
        let wb = message(&s, &b.recipient, &format!("approve {second}"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = [wa, wb]
            .into_iter()
            .map(|w| {
                let s = s.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    s.handle_control(&w).unwrap()
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|r| r.contains(": approved.")).count(),
            1
        );
        assert_eq!(reviewers(&s.db.lock().unwrap()).unwrap().len(), 1);
    }
    #[test]
    fn admin_changes_are_durable_and_old_proposals_cannot_survive_membership_cycles() {
        let (d, s, a, b, c) = setup();
        let stale = propose(
            &s,
            &b.user,
            Change::AddApprovalAdmin {
                user: c.user.clone(),
            },
        );
        let remove = propose(
            &s,
            &a.user,
            Change::RemoveApprovalAdmin {
                user: b.user.clone(),
            },
        );
        approve(&s, &a.recipient, &remove);
        let add = propose(
            &s,
            &a.user,
            Change::AddApprovalAdmin {
                user: b.user.clone(),
            },
        );
        approve(&s, &a.recipient, &add);
        assert_eq!(
            s.proposal_status(&a.user, &stale).unwrap()["state"],
            "stale"
        );
        assert!(approve(&s, &a.recipient, &stale).contains("already decided"));
        let revision = s.setting("approval_admin_revision").unwrap();
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.setting("approval_admin_revision").unwrap(), revision);
        assert!(is_reviewer(&s.db.lock().unwrap(), &b.user).unwrap());
        assert!(!is_reviewer(&s.db.lock().unwrap(), &c.user).unwrap());
        assert!(s.bootstrap_admins(&[c.user]).is_err());
    }
    #[test]
    fn admin_promotion_rechecks_the_exact_verified_identity() {
        let (_d, s, a, _, c) = setup();
        let id = propose(
            &s,
            &a.user,
            Change::AddApprovalAdmin {
                user: c.user.clone(),
            },
        );
        s.db.lock()
            .unwrap()
            .execute(
                "UPDATE identities SET identity='changed@test' WHERE user_id=?",
                [&c.user],
            )
            .unwrap();
        assert!(approve(&s, &a.recipient, &id).contains(": stale."));
        assert!(!is_reviewer(&s.db.lock().unwrap(), &c.user).unwrap());
        s.db.lock()
            .unwrap()
            .execute(
                "INSERT INTO identities VALUES('ambiguous@test',?)",
                [&c.user],
            )
            .unwrap();
        assert!(s
            .propose(&a.user, Change::AddApprovalAdmin { user: c.user })
            .is_err());
    }
    #[test]
    fn quoted_approval_is_bound_to_recipient_and_survives_restart() {
        let (d, s, a, b, c) = setup();
        let id = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Community".into(),
            },
        );
        let (out, recipient, body) = s.claim_control().unwrap().unwrap();
        let stanza = s.control_stanza(out).unwrap();
        assert_eq!(body,"Create group *Community*?\nReply to this message with *approve* or *reject*.\nExpires in 24h.");
        let reply = |s: &Store, who: &str, quote: &str, allowed: bool, text: &str| {
            s.ingest_reply(who, &Uuid::new_v4().to_string(), text, allowed, Some(quote))
                .unwrap();
            s.handle_control(&s.next_control().unwrap().unwrap())
                .unwrap()
        };
        let other = if recipient == a.recipient {
            &b.recipient
        } else {
            &a.recipient
        };
        assert!(reply(&s, other, &stanza, true, "approve").contains("Use WhatsApp Reply"));
        assert!(reply(&s, &c.recipient, &stanza, true, "approve").contains("sealed reviewer"));
        assert!(reply(&s, &recipient, "forged", true, "approve").contains("Use WhatsApp Reply"));
        assert!(reply(&s, &recipient, &stanza, false, "approve").contains("Forwarded"));
        assert!(
            reply(&s, &recipient, &stanza, true, "approve please").contains("Use WhatsApp Reply")
        );
        assert_eq!(
            s.approval_requests().unwrap()["requests"][0]["state"],
            "pending"
        );
        s.ingest_reply(&recipient, "durable-quote", "ApPrOvE", true, Some(&stanza))
            .unwrap();
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let w = s.next_control().unwrap().unwrap();
        assert!(s.handle_control(&w).unwrap().contains(": approved."));
        assert!(reply(&s, &recipient, &stanza, true, "approve").contains("already decided"));
        let data = s.approval_requests().unwrap();
        assert_eq!(data["requests"][0]["state"], "approved");
        assert!(!data.to_string().contains(&id));
    }
    #[test]
    fn old_confirmed_notices_are_upgraded_but_uncertain_sends_are_not_replayed() {
        let (d, s, a, _, _) = setup();
        propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Legacy".into(),
            },
        );
        {
            let db = s.db.lock().unwrap();
            db.execute("UPDATE control_outbox SET text='old notice',state='sent' WHERE id=(SELECT MIN(id) FROM control_outbox)",[]).unwrap();
            db.execute(
                "UPDATE control_outbox SET text='uncertain',state='unknown' WHERE state='pending'",
                [],
            )
            .unwrap();
        }
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let (out, _, body) = s.claim_control().unwrap().unwrap();
        assert!(body.starts_with("Create group *Legacy*?"));
        s.control_sent(out, true).unwrap();
        assert!(s.claim_control().unwrap().is_none());
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert!(s.claim_control().unwrap().is_none());
    }
    #[test]
    fn one_admin_can_explicitly_approve_self_and_other_participants() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let admin = message(&s, "admin@test", "hello");
        let user = message(&s, "user@test", "hello");
        s.bootstrap_admins(std::slice::from_ref(&admin.user))
            .unwrap();
        let id = propose(
            &s,
            &admin.user,
            Change::CreateGroup {
                name: "Team".into(),
            },
        );
        assert!(s.permission_directory(&admin.user).unwrap()["groups"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(approve(&s, &user.recipient, &id).contains("sealed reviewer"));
        assert!(approve(&s, &admin.recipient, &id).contains(": approved."));
        assert!(approve(&s, &admin.recipient, &id).contains("already decided"));
        let note = s.save_note(&user.user, "shared", "hello").unwrap();
        let share = propose(
            &s,
            &user.user,
            Change::GrantRead {
                resource: note.clone(),
                audience: Audience::Everyone,
            },
        );
        assert!(s.read_note(&admin.user, &note).is_err());
        assert!(approve(&s, &admin.recipient, &share).contains(": approved."));
        assert_eq!(s.read_note(&admin.user, &note).unwrap(), "hello");
    }
    #[test]
    fn pending_review_discovery_filters_identity_and_expiry() {
        let (_d, s, a, b, c) = setup();
        let id = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Team".into(),
            },
        );
        assert_eq!(
            s.pending_reviews(&a.user).unwrap()["requests"][0]["can_review"],
            true
        );
        assert!(s.pending_reviews(&c.user).unwrap()["requests"]
            .as_array()
            .unwrap()
            .is_empty());
        let result = s.pending_reviews(&b.user).unwrap();
        assert!(result["requests"][0]["approval_instruction"]
            .as_str()
            .unwrap()
            .contains("WhatsApp Reply"));
        assert!(result["requests"][0].get("approval_command").is_none());
        s.db.lock()
            .unwrap()
            .execute(
                "UPDATE proposals SET expires=unixepoch()-1 WHERE id=?",
                [id],
            )
            .unwrap();
        assert!(s.pending_reviews(&b.user).unwrap()["requests"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn folder_inheritance_moves_revocation_and_cycles_are_reviewed() {
        let (_d, s, a, b, c) = setup();
        let folder = s.create_folder(&a.user, "Shared folder").unwrap();
        let child = s.create_folder(&a.user, "Nested folder").unwrap();
        let note = s.save_note(&a.user, "Instructions", "Bring water").unwrap();
        let move_child = propose(
            &s,
            &a.user,
            Change::MoveResource {
                resource: child.clone(),
                parent: Some(folder.clone()),
            },
        );
        approve(&s, &b.recipient, &move_child);
        let move_note = propose(
            &s,
            &a.user,
            Change::MoveResource {
                resource: note.clone(),
                parent: Some(child.clone()),
            },
        );
        approve(&s, &b.recipient, &move_note);
        assert!(s.read_note(&c.user, &note).is_err());
        assert!(s
            .propose(
                &a.user,
                Change::MoveResource {
                    resource: folder.clone(),
                    parent: Some(child.clone())
                }
            )
            .is_err());
        assert!(s
            .propose(
                &c.user,
                Change::MoveResource {
                    resource: note.clone(),
                    parent: None
                }
            )
            .is_err());
        let share = propose(
            &s,
            &a.user,
            Change::GrantRead {
                resource: folder.clone(),
                audience: Audience::User { id: c.user.clone() },
            },
        );
        approve(&s, &b.recipient, &share);
        assert_eq!(s.read_note(&c.user, &note).unwrap(), "Bring water");
        assert_eq!(s.notes(&c.user).unwrap().as_array().unwrap().len(), 3);
        let revoke = propose(
            &s,
            &a.user,
            Change::RevokeRead {
                resource: folder.clone(),
                audience: Audience::User { id: c.user.clone() },
            },
        );
        approve(&s, &b.recipient, &revoke);
        assert!(s.read_note(&c.user, &note).is_err());
        let share = propose(
            &s,
            &a.user,
            Change::GrantRead {
                resource: folder,
                audience: Audience::Everyone,
            },
        );
        approve(&s, &b.recipient, &share);
        let out = propose(
            &s,
            &a.user,
            Change::MoveResource {
                resource: child,
                parent: None,
            },
        );
        approve(&s, &b.recipient, &out);
        assert!(s.read_note(&c.user, &note).is_err());
    }
    #[test]
    fn moving_derived_content_cannot_bypass_source_restrictions() {
        let (_d, s, a, b, c) = setup();
        let public = s.create_folder(&c.user, "Public").unwrap();
        let share = propose(
            &s,
            &c.user,
            Change::GrantRead {
                resource: public.clone(),
                audience: Audience::Everyone,
            },
        );
        approve(&s, &a.recipient, &share);
        let source = s
            .save_note(&a.user, "Private source", "Keep this restricted")
            .unwrap();
        let grant = propose(
            &s,
            &a.user,
            Change::GrantRead {
                resource: source.clone(),
                audience: Audience::User { id: c.user.clone() },
            },
        );
        approve(&s, &b.recipient, &grant);
        s.read_note(&c.user, &source).unwrap();
        let derived = s
            .save_note(&c.user, "Summary", "Restricted summary")
            .unwrap();
        assert!(s
            .propose(
                &c.user,
                Change::MoveResource {
                    resource: derived,
                    parent: Some(public)
                }
            )
            .is_err());
    }
    #[test]
    fn admin_controls_are_exact_live_expiring_and_durable() {
        let (d, s, a, b, c) = setup();
        let id = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Readers".into(),
            },
        );
        assert!(approve(&s, &c.recipient, &id).contains("sealed reviewer"));
        assert!(s
            .handle_control(&message(&s, &b.recipient, "approve"))
            .unwrap()
            .contains("Use WhatsApp Reply"));
        assert!(s
            .handle_control(&message(&s, &b.recipient, "yes"))
            .is_none());
        s.ingest_transport(&b.recipient, "forwarded", &format!("approve {id}"), false)
            .unwrap();
        assert!(s
            .handle_control(&s.next().unwrap().unwrap())
            .unwrap()
            .contains("Forwarded"));
        let (_, recipient, notice) = s.claim_control().unwrap().unwrap();
        assert!([a.recipient.clone(), b.recipient.clone()].contains(&recipient));
        assert!(s.claim_control().unwrap().is_some());
        assert!(!notice.contains(&id));
        assert!(notice.contains("Create group *Readers*?"));
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert!(s.claim_control().unwrap().is_none()); // uncertain send is never replayed
        assert!(approve(&s, &b.recipient, &id).contains(": approved."));
        assert!(approve(&s, &b.recipient, &id).contains("already decided"));
        assert_eq!(
            s.permission_directory(&a.user).unwrap()["groups"][0]["id"],
            id
        );
        let expired = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Expired".into(),
            },
        );
        s.db.lock()
            .unwrap()
            .execute("UPDATE proposals SET expires=0 WHERE id=?", [&expired])
            .unwrap();
        assert!(approve(&s, &b.recipient, &expired).contains(": expired."));
        let rejected = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Rejected".into(),
            },
        );
        let w = message(&s, &b.recipient, &format!("reject {rejected}"));
        assert!(s.handle_control(&w).unwrap().contains(": rejected."));
        assert_eq!(
            s.permission_directory(&a.user).unwrap()["groups"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn sharing_and_revocation_cover_derived_notes_history_and_queued_output() {
        let (_d, s, a, b, c) = setup();
        let source = s.save_note(&a.user, "source", "private detail").unwrap();
        assert!(s.read_note(&c.user, &source).is_err());
        assert!(s.read_note(&b.user, &source).is_err()); // reviewer has no content override
        let grant = propose(
            &s,
            &a.user,
            Change::GrantRead {
                resource: source.clone(),
                audience: Audience::User { id: c.user.clone() },
            },
        );
        assert!(s.read_note(&c.user, &source).is_err()); // pending is not authority
        assert!(approve(&s, &b.recipient, &grant).contains(": approved."));
        assert_eq!(s.read_note(&c.user, &source).unwrap(), "private detail");
        let derivative = s
            .save_note(&c.user, "summary", "derived private detail")
            .unwrap();
        assert!(s
            .propose(
                &c.user,
                Change::GrantRead {
                    resource: derivative.clone(),
                    audience: Audience::Everyone
                }
            )
            .is_err());
        assert!(s
            .propose(
                &b.user,
                Change::GrantRead {
                    resource: source.clone(),
                    audience: Audience::Everyone
                }
            )
            .is_err());
        let pending = message(&s, &c.recipient, "summarize source");
        s.finish(&pending, "private detail").unwrap();
        let in_flight = message(&s, &c.recipient, "another request");
        let revoke = propose(
            &s,
            &a.user,
            Change::RevokeRead {
                resource: source.clone(),
                audience: Audience::User { id: c.user.clone() },
            },
        );
        assert!(approve(&s, &b.recipient, &revoke).contains(": approved."));
        assert!(s.read_note(&c.user, &source).is_err());
        assert!(s.read_note(&c.user, &derivative).is_err());
        assert_eq!(s.notes(&c.user).unwrap(), json!([]));
        assert!(s.history(&c.user).unwrap().is_empty());
        assert!(s.check_work(&in_flight).is_err());
        assert!(s.finish(&in_flight, "old context").is_err());
        assert!(s.claim_send().unwrap().is_none());
    }
    #[test]
    fn group_membership_is_reviewed_stale_safe_and_cannot_self_elevate() {
        let (_d, s, a, b, c) = setup();
        let g = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Team".into(),
            },
        );
        approve(&s, &b.recipient, &g);
        let add = propose(
            &s,
            &a.user,
            Change::AddMember {
                group: g.clone(),
                user: c.user.clone(),
            },
        );
        let stale = propose(
            &s,
            &a.user,
            Change::SetGroupAdmin {
                group: g.clone(),
                enabled: true,
            },
        );
        approve(&s, &b.recipient, &add);
        assert!(approve(&s, &b.recipient, &stale).contains(": stale."));
        assert!(s
            .propose(
                &c.user,
                Change::CreateGroup {
                    name: "Forbidden".into()
                }
            )
            .is_err());
        let privilege = propose(
            &s,
            &a.user,
            Change::SetGroupAdmin {
                group: g.clone(),
                enabled: true,
            },
        );
        approve(&s, &b.recipient, &privilege);
        assert!(
            s.permission_directory(&c.user).unwrap()["can_manage_groups"]
                .as_bool()
                .unwrap()
        );
        assert!(s
            .propose(
                &c.user,
                Change::AddMember {
                    group: g.clone(),
                    user: b.user.clone()
                }
            )
            .is_err());
        assert!(s
            .propose(
                &c.user,
                Change::SetGroupAdmin {
                    group: g.clone(),
                    enabled: false
                }
            )
            .is_err());
        let delegated = propose(
            &s,
            &c.user,
            Change::CreateGroup {
                name: "Delegated".into(),
            },
        );
        let remove = propose(
            &s,
            &a.user,
            Change::RemoveMember {
                group: g,
                user: c.user.clone(),
            },
        );
        approve(&s, &b.recipient, &remove);
        assert!(approve(&s, &a.recipient, &delegated).contains(": stale."));
    }
    #[test]
    fn group_and_everyone_grants_survive_restart_and_review_has_reserved_admission() {
        let (d, mut s, a, b, c) = setup();
        let g = propose(
            &s,
            &a.user,
            Change::CreateGroup {
                name: "Members".into(),
            },
        );
        approve(&s, &b.recipient, &g);
        let note = s.save_note(&a.user, "public", "welcome").unwrap();
        let grant = propose(
            &s,
            &a.user,
            Change::GrantRead {
                resource: note.clone(),
                audience: Audience::Group { id: g.clone() },
            },
        );
        approve(&s, &b.recipient, &grant);
        assert!(s.read_note(&c.user, &note).is_err());
        let add = propose(
            &s,
            &a.user,
            Change::AddMember {
                group: g,
                user: c.user.clone(),
            },
        );
        approve(&s, &b.recipient, &add);
        assert_eq!(s.read_note(&c.user, &note).unwrap(), "welcome");
        let public = propose(
            &s,
            &a.user,
            Change::GrantRead {
                resource: note.clone(),
                audience: Audience::Everyone,
            },
        );
        s.daily_user_limit = 0;
        s.daily_global_limit = 0;
        assert!(s
            .ingest(&c.recipient, "blocked", &format!("approve {public}"))
            .is_err());
        s.ingest(&b.recipient, "allowed", &format!("approve {public}"))
            .unwrap();
        let w = s.next_control().unwrap().unwrap();
        assert!(s.handle_control(&w).unwrap().contains(": approved."));
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let newcomer = message(&s, "new@test", "hi");
        assert_eq!(s.read_note(&newcomer.user, &note).unwrap(), "welcome");
    }
}
