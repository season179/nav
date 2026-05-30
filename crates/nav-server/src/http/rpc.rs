use nav_protocol::rpc::methods;

pub const KNOWN_METHODS: &[&str] = &[
    methods::INITIALIZE,
    methods::SESSION_CREATE,
    methods::SESSION_SEND_MESSAGE,
    methods::SESSION_SEARCH,
    methods::SESSION_TOTALS,
    methods::RUN_CANCEL,
    methods::TOOL_APPROVE,
    methods::TOOL_REJECT,
    methods::SESSION_CLOSE,
];

pub const ROUTED_METHODS: &[&str] = &[
    methods::INITIALIZE,
    methods::SESSION_CREATE,
    methods::SESSION_SEND_MESSAGE,
    methods::SESSION_SEARCH,
    methods::SESSION_TOTALS,
    methods::RUN_CANCEL,
    methods::TOOL_APPROVE,
    methods::TOOL_REJECT,
    methods::SETTINGS_RELOAD,
];
