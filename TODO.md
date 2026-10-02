1. When clicking a gear icon in chat top bar show AI Chat settings modal that has a button to show a "Model providers" settings, "Permissions & Approval" and go back button that is always visible under modal header
2. When deleting a model provider, show a confirmation dialog
3. Don't create a new chat if current chat is empty(has 0 messages)
4. On failed messages response or sending we send only message without a context: opened file, line and so on. Need to send a context as well
5. Chat title generation is broken, it's not generating any title, fix it

Settings UI: Permissions & Approval

In Chat Settings, add a “Permissions & Approval” a dedicated button that opens a view

1. Allowed commands

   Show a list of allowed commands (e.g., ls, git status, npm test, etc.).

   Provide a checkbox:

        “Allow any command”

            If checked:

                Hide the list of allowed commands (no need to curate).

                All commands are considered allowed at the permission level.

            If unchecked:

                Show the list of allowed commands.

                Only commands in this list are allowed; others are blocked at the permission layer.

This is purely a permission control, not an approval control. 2. File changes without approval

    Add a checkbox:

        “Allow file changes without approval”

            If checked: any allowed command that modifies files can run without an approval step.

            If unchecked: file-modifying commands go through the approval flow (manual or AI-based, as configured below).

You can detect “file-modifying” commands via a simple rule set or metadata on each tool/command. 3. Tool execution approval

    Add a checkbox:

        “Enable tool execution approval”

            If unchecked:

                No approval step for any allowed command (subject to the file-changes setting above if you want to keep that separation).

            If checked:

                Show two radio buttons for Approval mode:

                    Manual – every approval-required action pauses and asks the user to approve/reject.

                    By AI – every approval-required action is automatically evaluated by an AI model; the user sees the decision but does not manually approve each time (for now).

“By AI” model selection

    If “By AI” is selected:

        Show a list of user-added models (e.g., typesafe-ai/jev, openrouter/..., or any custom model the user has configured).

        Let the user:

            Select a default approval model.

            Optionally mark some models as fallbacks (later).

    If no models are configured yet, show a placeholder message like:

        “No approval models configured. Add models in the Models settings, then select one here.”
