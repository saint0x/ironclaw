-- Aria Registries: Full SDK surface area
-- V4: Tool, Agent, Memory, Task, Feed, Cron, KV, Team, Pipeline, Container, Network registries

-- ==================== Aria: Tool Registry ====================

CREATE TABLE aria_tools (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    parameters_schema JSONB NOT NULL DEFAULT '{}',
    handler_code TEXT NOT NULL,
    handler_hash TEXT NOT NULL,
    version INT NOT NULL DEFAULT 1,
    sandbox_config JSONB,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_tool_per_tenant UNIQUE (tenant_id, name)
);

CREATE INDEX idx_aria_tools_tenant ON aria_tools(tenant_id) WHERE status = 'active';
CREATE INDEX idx_aria_tools_name ON aria_tools(tenant_id, name) WHERE status = 'active';

-- ==================== Aria: Agent Registry ====================

CREATE TABLE aria_agents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    model TEXT,
    system_prompt TEXT,
    tools JSONB NOT NULL DEFAULT '[]',
    thinking_level TEXT,
    max_retries INT NOT NULL DEFAULT 3,
    timeout_secs INT,
    runtime_injections JSONB NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_agent_per_tenant UNIQUE (tenant_id, name)
);

CREATE INDEX idx_aria_agents_tenant ON aria_agents(tenant_id) WHERE status = 'active';

-- ==================== Aria: Memory Registry (Tiered KV) ====================

CREATE TABLE aria_memory (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value JSONB NOT NULL,
    tier TEXT NOT NULL DEFAULT 'longterm',
    session_id TEXT,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_memory_entry UNIQUE (tenant_id, key, tier),
    CONSTRAINT valid_memory_tier CHECK (tier IN ('scratchpad', 'ephemeral', 'longterm'))
);

CREATE INDEX idx_aria_memory_tenant ON aria_memory(tenant_id);
CREATE INDEX idx_aria_memory_key ON aria_memory(tenant_id, key);
CREATE INDEX idx_aria_memory_tier ON aria_memory(tenant_id, tier);
CREATE INDEX idx_aria_memory_session ON aria_memory(session_id) WHERE session_id IS NOT NULL;
CREATE INDEX idx_aria_memory_expires ON aria_memory(expires_at) WHERE expires_at IS NOT NULL;

-- ==================== Aria: Task Registry ====================

CREATE TABLE aria_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    handler_code TEXT,
    params JSONB NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'pending',
    result JSONB,
    error_message TEXT,
    agent_id UUID,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT valid_task_status CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled'))
);

CREATE INDEX idx_aria_tasks_tenant ON aria_tasks(tenant_id);
CREATE INDEX idx_aria_tasks_status ON aria_tasks(tenant_id, status);
CREATE INDEX idx_aria_tasks_agent ON aria_tasks(agent_id) WHERE agent_id IS NOT NULL;

-- ==================== Aria: Feed Registry ====================

CREATE TABLE aria_feeds (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    handler_code TEXT NOT NULL,
    handler_hash TEXT NOT NULL,
    schedule TEXT NOT NULL DEFAULT '0 * * * *',
    refresh_seconds INT NOT NULL DEFAULT 3600,
    category TEXT NOT NULL DEFAULT 'general',
    retention JSONB NOT NULL DEFAULT '{"max_items": 100, "max_age_days": 30}',
    display JSONB NOT NULL DEFAULT '{"priority": 0}',
    status TEXT NOT NULL DEFAULT 'active',
    last_run_at TIMESTAMPTZ,
    next_run_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_feed_per_tenant UNIQUE (tenant_id, name)
);

CREATE INDEX idx_aria_feeds_tenant ON aria_feeds(tenant_id) WHERE status = 'active';
CREATE INDEX idx_aria_feeds_schedule ON aria_feeds(next_run_at) WHERE status = 'active';

-- Feed items produced by feed runs
CREATE TABLE aria_feed_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    feed_id UUID NOT NULL REFERENCES aria_feeds(id) ON DELETE CASCADE,
    run_id UUID NOT NULL,
    card_type TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    source TEXT,
    url TEXT,
    metadata JSONB NOT NULL DEFAULT '{}',
    item_timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_aria_feed_items_feed ON aria_feed_items(feed_id);
CREATE INDEX idx_aria_feed_items_tenant ON aria_feed_items(tenant_id);
CREATE INDEX idx_aria_feed_items_run ON aria_feed_items(run_id);
CREATE INDEX idx_aria_feed_items_created ON aria_feed_items(created_at DESC);

-- ==================== Aria: Cron Function Registry ====================

CREATE TABLE aria_cron_functions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    schedule_kind TEXT NOT NULL,
    schedule_data JSONB NOT NULL,
    session_target TEXT NOT NULL DEFAULT 'main',
    wake_mode TEXT NOT NULL DEFAULT 'next-heartbeat',
    payload_kind TEXT NOT NULL,
    payload_data JSONB NOT NULL,
    isolation JSONB,
    cron_job_id TEXT,
    agent_id TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_cron_per_tenant UNIQUE (tenant_id, name),
    CONSTRAINT valid_schedule_kind CHECK (schedule_kind IN ('at', 'every', 'cron')),
    CONSTRAINT valid_session_target CHECK (session_target IN ('main', 'isolated')),
    CONSTRAINT valid_payload_kind CHECK (payload_kind IN ('system_event', 'agent_turn'))
);

CREATE INDEX idx_aria_cron_tenant ON aria_cron_functions(tenant_id);
CREATE INDEX idx_aria_cron_job_id ON aria_cron_functions(cron_job_id) WHERE cron_job_id IS NOT NULL;

-- Cron run log
CREATE TABLE aria_cron_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cron_function_id UUID NOT NULL REFERENCES aria_cron_functions(id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    status TEXT NOT NULL DEFAULT 'running',
    result JSONB,
    error_message TEXT
);

CREATE INDEX idx_aria_cron_runs_func ON aria_cron_runs(cron_function_id);
CREATE INDEX idx_aria_cron_runs_started ON aria_cron_runs(started_at DESC);

-- ==================== Aria: KV Registry ====================

CREATE TABLE aria_kv (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_kv_per_tenant UNIQUE (tenant_id, key)
);

CREATE INDEX idx_aria_kv_tenant ON aria_kv(tenant_id);
CREATE INDEX idx_aria_kv_key ON aria_kv(tenant_id, key);
CREATE INDEX idx_aria_kv_prefix ON aria_kv(tenant_id, key text_pattern_ops);

-- ==================== Aria: Team Registry ====================

CREATE TABLE aria_teams (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL DEFAULT 'coordinator',
    members JSONB NOT NULL DEFAULT '[]',
    shared_context JSONB NOT NULL DEFAULT '{}',
    timeout_secs INT,
    max_turns INT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_team_per_tenant UNIQUE (tenant_id, name),
    CONSTRAINT valid_team_mode CHECK (mode IN ('coordinator', 'round_robin', 'delegate', 'parallel', 'sequential'))
);

CREATE INDEX idx_aria_teams_tenant ON aria_teams(tenant_id) WHERE status = 'active';

-- ==================== Aria: Pipeline Registry ====================

CREATE TABLE aria_pipelines (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    steps JSONB NOT NULL DEFAULT '[]',
    variables JSONB NOT NULL DEFAULT '{}',
    timeout_secs INT,
    max_parallel INT NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_pipeline_per_tenant UNIQUE (tenant_id, name)
);

CREATE INDEX idx_aria_pipelines_tenant ON aria_pipelines(tenant_id) WHERE status = 'active';

-- Pipeline run tracking
CREATE TABLE aria_pipeline_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pipeline_id UUID NOT NULL REFERENCES aria_pipelines(id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL,
    variables JSONB NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'pending',
    current_step INT NOT NULL DEFAULT 0,
    step_results JSONB NOT NULL DEFAULT '[]',
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_aria_pipeline_runs_pipeline ON aria_pipeline_runs(pipeline_id);
CREATE INDEX idx_aria_pipeline_runs_status ON aria_pipeline_runs(status);

-- ==================== Aria: Container Registry ====================

CREATE TABLE aria_containers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    image TEXT NOT NULL,
    command TEXT,
    environment JSONB NOT NULL DEFAULT '{}',
    volumes JSONB NOT NULL DEFAULT '[]',
    ports JSONB NOT NULL DEFAULT '[]',
    resource_limits JSONB NOT NULL DEFAULT '{}',
    network_id UUID,
    labels JSONB NOT NULL DEFAULT '{}',
    restart_policy TEXT NOT NULL DEFAULT 'no',
    -- Runtime state (updated by execution layer)
    state TEXT NOT NULL DEFAULT 'pending',
    runtime_ip TEXT,
    runtime_pid INT,
    config_hash TEXT,
    last_used_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    exited_at TIMESTAMPTZ,
    exit_code INT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_container_per_tenant UNIQUE (tenant_id, name),
    CONSTRAINT valid_container_state CHECK (state IN ('pending', 'starting', 'running', 'stopped', 'exited', 'error')),
    CONSTRAINT valid_restart_policy CHECK (restart_policy IN ('no', 'always', 'on_failure', 'unless_stopped'))
);

CREATE INDEX idx_aria_containers_tenant ON aria_containers(tenant_id) WHERE status = 'active';
CREATE INDEX idx_aria_containers_network ON aria_containers(network_id) WHERE network_id IS NOT NULL;
CREATE INDEX idx_aria_containers_state ON aria_containers(state) WHERE status = 'active';

-- ==================== Aria: Network Registry ====================

CREATE TABLE aria_networks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    driver TEXT NOT NULL DEFAULT 'bridge',
    isolation TEXT NOT NULL DEFAULT 'default',
    ipv6 BOOLEAN NOT NULL DEFAULT false,
    dns_config JSONB,
    labels JSONB NOT NULL DEFAULT '{}',
    options JSONB NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_aria_network_per_tenant UNIQUE (tenant_id, name)
);

CREATE INDEX idx_aria_networks_tenant ON aria_networks(tenant_id) WHERE status = 'active';

-- ==================== Triggers ====================

CREATE TRIGGER update_aria_tools_updated_at
    BEFORE UPDATE ON aria_tools FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_agents_updated_at
    BEFORE UPDATE ON aria_agents FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_memory_updated_at
    BEFORE UPDATE ON aria_memory FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_tasks_updated_at
    BEFORE UPDATE ON aria_tasks FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_feeds_updated_at
    BEFORE UPDATE ON aria_feeds FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_cron_functions_updated_at
    BEFORE UPDATE ON aria_cron_functions FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_kv_updated_at
    BEFORE UPDATE ON aria_kv FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_teams_updated_at
    BEFORE UPDATE ON aria_teams FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_pipelines_updated_at
    BEFORE UPDATE ON aria_pipelines FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_containers_updated_at
    BEFORE UPDATE ON aria_containers FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
CREATE TRIGGER update_aria_networks_updated_at
    BEFORE UPDATE ON aria_networks FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
