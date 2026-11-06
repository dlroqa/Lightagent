//! The terminal chat's slash commands, in the order `/help` lists them: one
//! list behind the chat's `/help` and the API's `GET /commands`, which the
//! desktop shows with where each command is in the app.

/// One slash command and what it does.
pub struct SlashCommand {
    pub name: &'static str,
    pub summary: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        summary: "List the commands",
    },
    SlashCommand {
        name: "tools",
        summary: "List the tools this chat can call",
    },
    SlashCommand {
        name: "skills",
        summary: "List the installed skills",
    },
    SlashCommand {
        name: "extensions",
        summary: "List, install or uninstall extensions",
    },
    SlashCommand {
        name: "onboard",
        summary: "Install a Markdown file as the profile's guidance; /onboard remove withdraws it",
    },
    SlashCommand {
        name: "reload",
        summary: "Reload extensions and tool settings between turns",
    },
    SlashCommand {
        name: "new",
        summary: "Start a fresh session in the same profile",
    },
    SlashCommand {
        name: "continue",
        summary: "Continue a run paused on its time budget",
    },
    SlashCommand {
        name: "stop",
        summary: "Stop the running turn",
    },
    SlashCommand {
        name: "update",
        summary: "Update Lightagent",
    },
    SlashCommand {
        name: "exit",
        summary: "Leave the chat",
    },
];
