# TODOs

- [ ] TODO: Revisit headless session input when a concrete need arises [headless_session_input](#headless_session_input)
- [ ] TODO: Add buffer-scoped Vim mode matching st [vim_mode](#vim_mode)
  - [ ] TODO: Establish terminal and OMP behavior [vim_research](#vim_research)
    - [ ] TODO: Catalog reference Vim interactions [vim_reference](#vim_reference)
    - [ ] TODO: Inspect the OMP re-render command [omp_replay](#omp_replay)
    - [ ] TODO: Trace OMP terminal output safely [omp_trace](#omp_trace)
    - [ ] TODO: Choose history and view representation [vim_history_design](#vim_history_design)
    - [ ] TODO: Choose application control mechanism [vim_control_design](#vim_control_design)
  - [ ] TODO: Make buffer history available for navigation [vim_history](#vim_history)
    - [ ] TODO: Retain 2,000 rows of buffer history [history_capacity](#history_capacity)
    - [ ] TODO: Expose historical buffer rows [history_access](#history_access)
    - [ ] TODO: Maintain a shared buffer viewport [history_views](#history_views)
    - [ ] TODO: Handle history changes during navigation [history_lifecycle](#history_lifecycle)
    - [ ] TODO: Verify buffer history behavior [history_verify](#history_verify)
  - [ ] TODO: Implement buffer-scoped mode transitions [vim_transitions](#vim_transitions)
    - [ ] TODO: Implement mode entry shortcuts [vim_entry](#vim_entry)
    - [ ] TODO: Implement Normal mode exits [vim_exit](#vim_exit)
    - [ ] TODO: Implement Visual mode entry [visual_entry](#visual_entry)
    - [ ] TODO: Implement Visual cancellation [visual_escape](#visual_escape)
    - [ ] TODO: Verify mode transitions and passthrough [transitions_verify](#transitions_verify)
  - [ ] TODO: Implement reference-style navigation [vim_navigation](#vim_navigation)
    - [ ] TODO: Implement cursor motions and counts [vim_motions](#vim_motions)
    - [ ] TODO: Implement viewport scrolling [vim_scrolling](#vim_scrolling)
    - [ ] TODO: Render navigation feedback [vim_feedback](#vim_feedback)
    - [ ] TODO: Verify navigation behavior [navigation_verify](#navigation_verify)
  - [ ] TODO: Implement interactive history search [vim_search](#vim_search)
    - [ ] TODO: Define search inside Visual mode [search_visual_design](#search_visual_design)
    - [ ] TODO: Implement search prompt transitions [search_prompt](#search_prompt)
    - [ ] TODO: Implement search and match navigation [search_matches](#search_matches)
    - [ ] TODO: Verify search behavior [search_verify](#search_verify)
  - [ ] TODO: Implement Visual selection and yank [vim_selection](#vim_selection)
    - [ ] TODO: Implement Visual selection ranges [selection_ranges](#selection_ranges)
    - [ ] TODO: Render Visual selection [selection_render](#selection_render)
    - [ ] TODO: Copy Visual selection [selection_yank](#selection_yank)
    - [ ] TODO: Verify selection and copying [selection_verify](#selection_verify)
  - [ ] TODO: Support application-controlled Vim bindings [vim_app_control](#vim_app_control)
    - [ ] TODO: Implement buffer-scoped binding control [app_control](#app_control)
    - [ ] TODO: Integrate Neovim lifecycle [nvim_hook](#nvim_hook)
    - [ ] TODO: Verify application binding isolation [app_control_verify](#app_control_verify)
  - [ ] TODO: Validate and document the complete workflow [vim_acceptance](#vim_acceptance)
    - [ ] TODO: Compare behavior with plain st [vim_compare](#vim_compare)
    - [ ] TODO: Verify OMP chat navigation [omp_acceptance](#omp_acceptance)
    - [ ] TODO: Verify Neovim and OMP isolation [nvim_acceptance](#nvim_acceptance)
    - [ ] TODO: Document Vim mode in repository docs [vim_docs](#vim_docs)
    - [ ] TODO: Bump rmux version for Vim mode [vim_version](#vim_version)
  - [ ] TODO: Make Vim bindings configurable [vim_config](#vim_config)
    - [ ] TODO: Define Vim binding configuration [vim_config_design](#vim_config_design)
    - [ ] TODO: Implement configurable Vim bindings [vim_config_impl](#vim_config_impl)
    - [ ] TODO: Verify configurable Vim bindings [vim_config_verify](#vim_config_verify)
- [ ] TODO: Add configurable per-window borders and gaps [window_borders](#window_borders)

## headless_session_input

- Scope: coordinated

Deferred idea only: do not implement now. Revisit if a concrete workflow requires interacting with an existing buffer without an attached terminal. First investigate whether the agent harness or application provides a suitable native interface. Raw terminal input is not an agent API: the receiving program may be busy, may have exited, or may interpret text and Enter differently; if the shell is active, submitted text could execute as a command. It does not confirm that an agent accepted or completed a request. A separate exec operation would start another process, not send a message to an existing agent, and is not justified merely to run shell commands. If a real use case remains, explore target session and buffer selection, input and submission semantics, and feedback before deciding whether to implement.

## vim_mode

- Scope: coordinated
- Difficulty: 2 kyu
- Children: vim_research & vim_history & vim_transitions & vim_navigation & vim_search & vim_selection & vim_app_control & vim_acceptance & vim_config

Provide Vim-style navigation, search and selection scoped to the focused buffer, following dotFiles st-meta-vim-full unless specified otherwise. Share mode, navigation, search and selection state across windows and attached clients displaying the same buffer; different buffers remain independent. Retain about 2,000 terminal history rows per buffer. Dividers, borders, gaps and unused-space padding are display decorations, not buffer contents: exclude them from history, search, selection and copied text. Insert mode passes ordinary input through; Alt+Escape enters Normal, Alt+d/f enter and scroll, and Ctrl+f opens search. Normal Escape/i returns to Insert; v, V and Ctrl+v enter character, line and block Visual modes. Visual Escape clears selection and returns to Normal; y copies and returns to Insert. Search from Visual retains the selection; comma and semicolon navigate matches. Applications can disable rmux Vim bindings for their buffer, leaving window-management shortcuts available; Neovim integration restores them on exit. OMP should remain navigable. Test initially with st -V; dynamic st control is future work, not a dependency. Do not implement mouse yanks. Difficulty: coordinated changes to terminal history, input, rendering and application control; exact protocol and search interaction need investigation.

## window_borders

- Scope: coordinated
- Dependency: configuration

Add optional individual borders around rmux windows, with configurable characters and distinct focused and unfocused colors, plus configurable gaps between adjacent borders or dividers. Preserve the existing single-divider layout as the default. Ensure borders and gaps are reflected consistently in rendering and shared buffer sizing, including small windows and multiple differently sized clients. Revisit and update border configuration when the implementation is designed.

## vim_research

- Scope: coordinated
- Difficulty: 4 kyu
- Uncertainty: high
- Children: vim_reference & omp_replay & omp_trace & vim_history_design & vim_control_design

Investigate the installed reference and actual OMP rendering, replay and alternate-screen behavior before selecting a history design. High uncertainty: cached sources may differ from the installed binary. No running user sessions should be disturbed.

## vim_reference

Record st bindings, motions, counts, search, selection and visual feedback, distinguishing agreed differences.

## omp_replay

Locate the user-provided re-render command and determine how it replays conversation output.

## omp_trace

Use an isolated OMP session to observe normal output, replay, resize and alternate-screen sequences without touching active user sessions.

## vim_history_design

Decide how bounded buffer history and shared buffer-scoped navigation interact with live output, resize and replay using observed OMP behavior. Keep rmux decorations outside buffer content.

## vim_control_design

Define buffer-targeted enable/disable and restoration semantics, including nested applications and abnormal exit, based on available mechanisms.

## vim_history

- Scope: coordinated
- Difficulty: 3 kyu
- Dependency: vim_history_design & omp_trace
- Children: history_capacity & history_access & history_views & history_lifecycle & history_verify

Retain bounded terminal history and provide one shared navigation view per buffer, rendered in every window and attached client displaying it, without disrupting live output processing. Keep dividers, borders, gaps and padding outside the history. Difficulty: resize, replay, alternate screens, concurrent output and multiple clients require careful handling. Depends on the history design and OMP observations.

## history_capacity

Set the agreed approximate 2,000-row bound and define how overflow evicts old rows.

## history_access

Provide historical terminal cells for buffer navigation while preserving attributes and the live screen; exclude rmux decorations.

## history_views

Store one navigation position per buffer, shared across its windows and attached clients; keep different buffers independent.

## history_lifecycle

Keep view positions and selections valid through output, eviction, reset, replay and resize.

## history_verify

Test bounds, shared views across windows and attached clients, isolation between buffers, differently sized windows, attach, live output, resize, resets and OMP replay as applicable. Verify decorations never enter history.

## vim_transitions

- Scope: coordinated
- Difficulty: 4 kyu
- Dependency: vim_history_design & vim_control_design
- Children: vim_entry & vim_exit & visual_entry & visual_escape & transitions_verify

Handle Insert, Normal and the three Visual modes per buffer without intercepting application input in Insert mode except agreed entry bindings. Share mode state between windows and attached clients displaying the same buffer, while keeping different buffers independent. Difficulty: state and input routing must remain consistent across clients. Depends on the view representation and application-control design.

## vim_entry

Implement Alt+Escape and Alt+d/f from Insert, including five-row scrolling.

## vim_exit

Return to Insert on Escape or i and restore ordinary input routing.

## visual_entry

Enter character, line or block Visual mode using v, V or Ctrl+v from Normal.

## visual_escape

Clear the selection and return to Normal on Escape.

## transitions_verify

Test each agreed transition, shared state across windows and clients, isolation between buffers and unbound application input.

## vim_navigation

- Scope: coordinated
- Difficulty: 4 kyu
- Dependency: history_views & transitions_verify & vim_reference
- Children: vim_motions & vim_scrolling & vim_feedback & navigation_verify

Reproduce relevant st motions, counts, scrolling and cursor feedback in a shared buffer history view. Difficulty: movement across history, wrapped rows and resize must remain consistent. Depends on history access and mode transitions.

## vim_motions

Implement reference movement commands and numeric counts over buffer history.

## vim_scrolling

Implement reference scrolling commands and keep the navigation cursor visible.

## vim_feedback

Render the shared navigation cursor and mode feedback in windows displaying the buffer without affecting different buffers.

## navigation_verify

Test motions, counts, scroll bounds, focus, shared buffer views across windows and attached clients, and isolation between buffers.

## vim_search

- Scope: coordinated
- Difficulty: 4 kyu
- Dependency: navigation_verify & vim_reference
- Children: search_visual_design & search_prompt & search_matches & search_verify

Reproduce reference search and highlighting in buffer content, sharing search state across views of the same buffer while preserving the originating Normal or Visual context. Exclude dividers, borders, gaps and padding from search. Difficulty: Visual search interaction needs explicit design. Depends on navigation and the reference behavior catalog.

## search_visual_design

Specify prompt cancellation, confirmation, match navigation and selection-anchor behavior with the user before implementation.

## search_prompt

Open search with Ctrl+f from Insert, Normal and Visual; return to the appropriate context on confirmation or cancellation.

## search_matches

Implement incremental smart-case search, match highlighting, comma forward and semicolon backward, plus applicable reference search bindings.

## search_verify

Test case sensitivity, both directions, no matches, prompt transitions, shared state across views, isolation between buffers, decoration exclusion and Visual selection retention.

## vim_selection

- Scope: coordinated
- Difficulty: 4 kyu
- Dependency: navigation_verify & visual_entry
- Children: selection_ranges & selection_render & selection_yank & selection_verify

Select and copy only the focused buffer content, with character, line and rectangular shapes and reference-style feedback. Share selection across windows and clients displaying that buffer; exclude dividers, borders, gaps and padding. Difficulty: terminal cells, wide characters and line boundaries affect copied text. Depends on navigation and Visual entry.

## selection_ranges

Track character, whole-line and rectangular selection from anchor to cursor.

## selection_render

Highlight selected buffer cells in every window displaying that buffer, without highlighting decorations or different buffers.

## selection_yank

Yank the selected text to the clipboard, provide feedback and return to Insert.

## selection_verify

Test each shape, reversed endpoints, text boundaries, clipboard content, shared selection across windows and clients, and isolation between buffers. Verify differently sized windows and scrolling never select or copy dividers, borders, gaps or padding.

## vim_app_control

- Scope: coordinated
- Difficulty: 3 kyu
- Dependency: vim_control_design
- Children: app_control & nvim_hook & app_control_verify

Provide buffer-scoped enable and disable controls and a Neovim hook, without disabling rmux window shortcuts. Difficulty: restoration, nesting and unexpected exits need reliable semantics. Depends on application-control design.

## app_control

Provide an application-accessible control for disabling and restoring rmux Vim bindings in the correct buffer.

## nvim_hook

Provide a minimal Neovim Lua integration that disables rmux Vim bindings while active and restores them on exit.

## app_control_verify

Test Neovim lifecycle, restoration and other buffers, including relevant failure and nesting cases.

## vim_acceptance

- Scope: coordinated
- Difficulty: 4 kyu
- Dependency: vim_navigation & vim_search & vim_selection & vim_app_control & vim_config
- Children: vim_compare & omp_acceptance & nvim_acceptance & vim_docs & vim_version

Compare real st and rmux interactions, verify OMP and Neovim end to end, document the feature and bump the package version. Difficulty: terminal lifecycle and real application behavior need integration testing. Depends on navigation, search, selection, application control and configurable bindings.

## vim_compare

Exercise matching navigation, search and selection scenarios in st and rmux using st -V for rmux.

## omp_acceptance

Navigate, search and copy earlier OMP conversation output while OMP is running, including after re-render.

## nvim_acceptance

Confirm Neovim receives its bindings while OMP remains navigable in another buffer.

## vim_docs

- Dependency: vim_config & vim_compare & omp_acceptance & nvim_acceptance

Document Vim mode in the repository docs, including default and configurable bindings with an n and Shift+n search example, 2,000-row history, shared buffer-scoped navigation and selection, application control, st -V setup and intentional differences from st.

## vim_config

- Scope: coordinated
- Difficulty: 4 kyu
- Dependency: vim_navigation & vim_search & vim_selection
- Children: vim_config_design & vim_config_impl & vim_config_verify

Allow users to configure Vim-mode actions and keybindings using the existing rmux configuration. Preserve the agreed st-style bindings as defaults, while supporting alternatives such as n and Shift+n for search matches. Difficulty: mode-specific input and conflicts with other bindings need a clear resolution. Depends on the Vim navigation, search and selection behavior.

## vim_config_design

Specify how users map keys to Vim actions, including mode-specific bindings and conflicts, consistently with existing rmux configuration.

## vim_config_impl

- Dependency: vim_config_design

Apply configured Vim bindings while retaining the agreed defaults for omitted settings.

## vim_config_verify

- Dependency: vim_config_impl

Test default bindings and overrides, including n and Shift+n for next and previous search matches, and mode-specific conflicts.

## vim_version

- Dependency: vim_docs

After implementation, verification and documentation, update the package version in Cargo.toml and affected lockfile metadata according to the repository Semantic Versioning policy; verify the resulting package version.

# DONEs

- [x] DONE: Deliver persistent rmux sessions [rmux](#rmux)
  - [x] DONE: Implement and integrate rmux [delivery](#delivery)
    - [x] DONE: Build and verify the Bash CLI [cli](#cli)
      - [x] DONE: Choose private runtime socket storage [runtime](#runtime)
      - [x] DONE: Parse and validate CLI arguments [arguments](#arguments)
      - [x] DONE: Implement safe socket liveness handling [liveness](#liveness)
      - [x] DONE: Implement session commands [sessions](#sessions)
      - [x] DONE: Test disconnect and reconnect [reconnect](#reconnect)
      - [x] DONE: Test live and stale sockets [stale](#stale)
      - [x] DONE: Test command and working directory [cwd](#cwd)
      - [x] DONE: Test attach-or-create behavior [up](#up)
      - [x] DONE: Test session exit and reuse [exit](#exit)
    - [x] DONE: Integrate and verify dotfiles launchers [integration](#integration)
      - [x] DONE: Make rmux available to launchers [path](#path)
      - [x] DONE: Switch the terminal launcher [terminal](#terminal)
      - [x] DONE: Switch the OMP launcher [omp](#omp)
      - [x] DONE: Test the real launcher paths [launcher_test](#launcher_test)
      - [x] DONE: Document rmux usage [docs](#docs)

- [x] DONE: Deliver named session termination [kill_feature](#kill_feature)
  - [x] DONE: Implement session termination [kill_change](#kill_change)
    - [x] DONE: Build and verify the kill command [kill_session](#kill_session)
      - [x] DONE: Determine safe termination mechanism [kill_method](#kill_method)
      - [x] DONE: Implement the kill command [kill_command](#kill_command)
      - [x] DONE: Update command usage [kill_usage](#kill_usage)
      - [x] DONE: Test terminating a live session [kill_live_test](#kill_live_test)
      - [x] DONE: Test attached clients and isolation [kill_clients_test](#kill_clients_test)
      - [x] DONE: Test termination error cases [kill_errors_test](#kill_errors_test)
      - [x] DONE: Test session lifecycle races [kill_race_test](#kill_race_test)

- [x] DONE: Select or create sessions in both launchers [session_picker](#session_picker)
  - [x] DONE: Deliver session selection for both launchers [picker_delivery](#picker_delivery)
    - [x] DONE: Implement and verify session selection [picker_work](#picker_work)
      - [x] DONE: Identify current session naming and creation [picker_naming](#picker_naming)
      - [x] DONE: List existing rmux sessions [picker_list](#picker_list)
      - [x] DONE: Filter sessions for the selected checkout [picker_filter](#picker_filter)
      - [x] DONE: Create a clean session when none exists [picker_empty](#picker_empty)
      - [x] DONE: Offer existing sessions and new session [picker_choice](#picker_choice)
      - [x] DONE: Create an additional suffixed session [picker_new](#picker_new)
      - [x] DONE: Verify both launcher workflows [picker_verify](#picker_verify)

- [x] DONE: Deliver interactive rmux sessions [interactive_mux](#interactive_mux)
  - [x] DONE: Decide the interactive architecture [design_stage](#design_stage)
    - [x] DONE: Determine the implementation architecture [architecture](#architecture)
      - [x] DONE: Inspect the existing CLI and launcher contracts [inspect_contract](#inspect_contract)
      - [x] DONE: Assess terminal emulation and multiplexer implementation options [assess_engines](#assess_engines)
      - [x] DONE: Assess whether dtach can support the required session model [assess_dtach](#assess_dtach)
      - [x] DONE: Inspect st key mappings and window-manager interception [inspect_st_keys](#inspect_st_keys)
      - [x] DONE: Choose the session, client and terminal architecture [select_design](#select_design)
  - [x] DONE: Deliver persistent session and buffer runtime [runtime_stage](#runtime_stage)
    - [x] DONE: Implement named session runtime [session_runtime](#session_runtime)
      - [x] DONE: Implement existing new, up, attach, ls and kill commands [preserve_cli](#preserve_cli)
      - [x] DONE: Create the persistent session process [session_server](#session_server)
      - [x] DONE: Connect an interactive client to a named session [client_attach](#client_attach)
      - [x] DONE: Keep the session running when a terminal disconnects [client_detach](#client_detach)
      - [x] DONE: Track simultaneous attached terminal clients [attachment_tracking](#attachment_tracking)
      - [x] DONE: Clean up session resources after session termination [session_cleanup](#session_cleanup)
      - [x] DONE: Adapt affected terminal and OMP launchers to the new runtime [launcher_migration](#launcher_migration)
    - [x] DONE: Implement terminal buffers [buffer_runtime](#buffer_runtime)
      - [x] DONE: Start a terminal program for a new buffer [buffer_process](#buffer_process)
      - [x] DONE: Maintain the terminal screen state for each buffer [terminal_state](#terminal_state)
      - [x] DONE: Keep an undisplayed buffer running [hidden_buffer](#hidden_buffer)
      - [x] DONE: Maintain the ordered buffer list [buffer_order](#buffer_order)
      - [x] DONE: Terminate the selected buffer and its program [buffer_close](#buffer_close)
      - [x] DONE: Remove a buffer when its program exits [buffer_exit](#buffer_exit)
      - [x] DONE: End the session when its last buffer closes [final_buffer](#final_buffer)
  - [x] DONE: Deliver shared interactive views [interface_stage](#interface_stage)
    - [x] DONE: Implement shared rmux windows [window_layout](#window_layout)
      - [x] DONE: Display the initial buffer in one rmux window [initial_window](#initial_window)
      - [x] DONE: Split a window vertically showing its existing buffer [vertical_split](#vertical_split)
      - [x] DONE: Split a window horizontally showing its existing buffer [horizontal_split](#horizontal_split)
      - [x] DONE: Close an rmux window without closing its buffer [close_window](#close_window)
      - [x] DONE: Ignore window-close requests for the final rmux window [last_window](#last_window)
      - [x] DONE: Move focus between adjacent windows by direction [focus_window](#focus_window)
      - [x] DONE: Move the focused window in the requested direction [move_window](#move_window)
      - [x] DONE: Resize the focused window in the requested direction [resize_window](#resize_window)
      - [x] DONE: Switch the focused window to the previous or next buffer [switch_buffer](#switch_buffer)
      - [x] DONE: Move the focused buffer left or right in the buffer list [reorder_buffer](#reorder_buffer)
      - [x] DONE: Show a newly created buffer in the focused window [new_buffer_view](#new_buffer_view)
      - [x] DONE: Replace a closed buffer in every window displaying it [closed_buffer_views](#closed_buffer_views)
    - [x] DONE: Render shared sessions across attached terminals [presentation](#presentation)
      - [x] DONE: Render the shared window layout for each attached terminal [render_layout](#render_layout)
      - [x] DONE: Render the ordered buffer line with the focused buffer identified [buffer_line](#buffer_line)
      - [x] DONE: Compute each displayed buffer size from the minimum view width and height across all clients [buffer_geometry](#buffer_geometry)
      - [x] DONE: Notify the buffer program when its terminal size changes [notify_resize](#notify_resize)
      - [x] DONE: Center each buffer screen within a larger rmux window [centered_view](#centered_view)
      - [x] DONE: Fill unused window space with a background pattern [background_pattern](#background_pattern)
      - [x] DONE: Recalculate buffer sizes when views, attachments or terminal sizes change [geometry_updates](#geometry_updates)
      - [x] DONE: Propagate shared layout, focus and buffer changes to all attached clients [shared_redraw](#shared_redraw)
    - [x] DONE: Implement interactive keyboard controls [input_controls](#input_controls)
      - [x] DONE: Choose a distinguishable input encoding and application key-passthrough policy [key_protocol](#key_protocol)
      - [x] DONE: Bind Ctrl-T and Ctrl-W to buffer creation and closure [buffer_keys](#buffer_keys)
      - [x] DONE: Bind Ctrl-H and Ctrl-L to buffer navigation [buffer_navigation_keys](#buffer_navigation_keys)
      - [x] DONE: Bind Ctrl-Shift-H and Ctrl-Shift-L to buffer reordering [buffer_move_keys](#buffer_move_keys)
      - [x] DONE: Bind Alt-V and Alt-B to vertical and horizontal splits [window_split_keys](#window_split_keys)
      - [x] DONE: Bind Alt-W to closing an rmux window [window_close_key](#window_close_key)
      - [x] DONE: Bind Alt-H, J, K and L to directional window focus [window_focus_keys](#window_focus_keys)
      - [x] DONE: Bind Alt-Shift directional keys to window movement [window_move_keys](#window_move_keys)
      - [x] DONE: Bind Alt-Ctrl directional keys to window resizing [window_size_keys](#window_size_keys)
      - [x] DONE: Update st key handling if the chosen bindings require it [st_integration](#st_integration)
    - [x] DONE: Add user-visible names for terminal buffers [buffer_rename](#buffer_rename)
      - [x] DONE: Track display names alongside buffer IDs [buffer_name_state](#buffer_name_state)
      - [x] DONE: Rename the active buffer with Ctrl-R [rename_buffer_key](#rename_buffer_key)
    - [x] DONE: Equalize the sizes of rmux windows [equalize_windows](#equalize_windows)
      - [x] DONE: Rebalance split ratios across visible windows [equalize_layout](#equalize_layout)
      - [x] DONE: Bind window equalization to Alt+= [equalize_key](#equalize_key)
  - [x] DONE: Verify and document the delivered interface [acceptance_stage](#acceptance_stage)
    - [x] DONE: Verify the complete interactive contract [verification](#verification)
      - [x] DONE: Verify the existing CLI creation, attachment, listing and kill behavior [verify_cli](#verify_cli)
      - [x] DONE: Verify the real terminal and OMP launcher paths [verify_launchers](#verify_launchers)
      - [x] DONE: Verify session and buffers survive terminal disconnect and reattach [verify_detach](#verify_detach)
      - [x] DONE: Verify a hidden buffer keeps running and can be displayed again [verify_hidden](#verify_hidden)
      - [x] DONE: Verify both split directions initially show the same live buffer [verify_splits](#verify_splits)
      - [x] DONE: Verify switching one rmux window leaves another window buffer unchanged [verify_independent_switch](#verify_independent_switch)
      - [x] DONE: Verify closing windows preserves buffers and the last window stays open [verify_window_close](#verify_window_close)
      - [x] DONE: Verify buffer closure updates all views and the last buffer ends the session [verify_buffer_close](#verify_buffer_close)
      - [x] DONE: Verify buffer line navigation and reordering [verify_buffer_line](#verify_buffer_line)
      - [x] DONE: Verify directional window focus, movement and resizing [verify_window_controls](#verify_window_controls)
      - [x] DONE: Verify two attachments share layout, focus and actions [verify_shared_clients](#verify_shared_clients)
      - [x] DONE: Verify minimum buffer dimensions across views and differently sized clients [verify_shared_size](#verify_shared_size)
      - [x] DONE: Verify centered screen placement and background in larger views [verify_centering](#verify_centering)
      - [x] DONE: Verify application dimensions update after resize, view changes and detach [verify_resize_events](#verify_resize_events)
      - [x] DONE: Verify the agreed key combinations through the actual st and i3 path [verify_keys](#verify_keys)
      - [x] DONE: Verify non-rmux input reaches interactive terminal applications [verify_application_input](#verify_application_input)
      - [x] DONE: Check whether rmux needs the custom st key patch [verify_st_patch](#verify_st_patch)
      - [x] DONE: Verify buffer rename behavior [verify_buffer_rename](#verify_buffer_rename)
      - [x] DONE: Verify rmux window equalization [verify_equalize](#verify_equalize)
      - [x] DONE: Verify Atuin cursor-position queries [verify_atuin](#verify_atuin)
      - [x] DONE: Verify first pane text colors [verify_pane_color](#verify_pane_color)
    - [x] DONE: Update user-facing usage [usage_docs](#usage_docs)
      - [x] DONE: Document session, buffer and rmux window terminology [document_terms](#document_terms)
      - [x] DONE: Document the supported keyboard controls and conflicts [document_controls](#document_controls)
      - [x] DONE: Document session, buffer and window lifetime behavior [document_lifecycle](#document_lifecycle)
      - [x] DONE: Document shared attachments and buffer sizing [document_views](#document_views)

- [x] DONE: Add --version and -v flags [version_flags](#version_flags)

- [x] DONE: Add user configuration for rmux [configuration](#configuration)
  - [x] DONE: Load TOML configuration with defaults [config_loading](#config_loading)
    - [x] DONE: Define configuration types and defaults [config_schema](#config_schema)
    - [x] DONE: Locate the configuration file [config_path](#config_path)
    - [x] DONE: Read the configuration file [config_read](#config_read)
    - [x] DONE: Parse TOML into configuration [config_parse](#config_parse)
    - [x] DONE: Validate configuration values [config_validate](#config_validate)
    - [x] DONE: Test configuration loading and errors [config_load_tests](#config_load_tests)
    - [x] DONE: Load configuration when rmux starts [config_startup](#config_startup)
    - [x] DONE: Test configuration across session startup and attachment [config_start_tests](#config_start_tests)
  - [x] DONE: Configure rmux keybindings [config_keys](#config_keys)
    - [x] DONE: Define keybinding syntax [keys_syntax](#keys_syntax)
    - [x] DONE: Parse configured keybindings [keys_parse](#keys_parse)
    - [x] DONE: Apply configured keybindings [keys_apply](#keys_apply)
    - [x] DONE: Test configurable keybindings [keys_tests](#keys_tests)
  - [x] DONE: Configure buffer-line colors [config_buffer_colors](#config_buffer_colors)
    - [x] DONE: Identify buffer visibility states [buffer_color_states](#buffer_color_states)
    - [x] DONE: Render configured buffer-line colors [buffer_color_render](#buffer_color_render)
    - [x] DONE: Test buffer-line colors [buffer_color_tests](#buffer_color_tests)
  - [x] DONE: Configure existing window dividers [config_dividers](#config_dividers)
    - [x] DONE: Render configured divider styles [divider_style](#divider_style)
    - [x] DONE: Test divider styles [divider_tests](#divider_tests)
  - [x] DONE: Configure unused-space fill [config_margin](#config_margin)
    - [x] DONE: Render configured margin fill [margin_style](#margin_style)
    - [x] DONE: Test margin fill [margin_tests](#margin_tests)
  - [x] DONE: Configure window resize step [config_resize](#config_resize)
    - [x] DONE: Apply configured window resize step [resize_apply](#resize_apply)
    - [x] DONE: Test configurable resize step [resize_tests](#resize_tests)
  - [x] DONE: Document user configuration [config_docs](#config_docs)
- [x] DONE: Configure buffer-line prefixes and suffixes [buffer_line_markers](#buffer_line_markers)
- [x] DONE: Investigate OMP multiline paste submission [omp_paste_submission](#omp_paste_submission)

- [x] DONE: Add word editing to buffer rename [rename_word_editing](#rename_word_editing)

- [x] DONE: Investigate OMP crash on resize [omp_resize_crash](#omp_resize_crash)
## rmux

- Role: material
- Scope: coordinated
- Difficulty: 3 kyu
- Children: delivery

Implement and integrate the full specification as one material. Difficulty lies in safe lifecycle management and real interactive launcher verification; there are no prerequisites.

## delivery

- Role: composite
- Scope: coordinated
- Difficulty: 4 kyu
- Children: cli & integration

One coherent change spanning the CLI, session tests, and dotfiles integration. CLI behavior must be verified before launcher integration.

## cli

- Role: molecule
- Scope: local
- Difficulty: 4 kyu
- Children: runtime & arguments & liveness & sessions & reconnect & stale & cwd & up & exit

Provide the specified dtach session lifecycle and verify it with real terminal clients. Liveness and creation races are the main risks; no prerequisites.

## runtime

- Role: atomic

Create user-specific runtime storage with safe permissions; sessions must not survive reboot.

## arguments

- Role: atomic

Implement new, ls, attach and up syntax; reject unsafe names and clearly report missing dtach.

## liveness

- Role: atomic

Distinguish live, stale and unreachable sockets; cleanup and concurrent creation must not disrupt live sessions.

## sessions

- Role: atomic

Create, list, attach and attach-or-create with dtach; preserve arguments and apply cwd and command only on creation.

## reconnect

- Role: atomic

Disconnect a terminal and reconnect to the same running application.

## stale

- Role: atomic

Check ls excludes stale sockets, stale names can be reused, live sessions are preserved, and creation races are safe.

## cwd

- Role: atomic

Verify argument boundaries, requested cwd, default shell and meaningful errors.

## up

- Role: atomic

Run up twice, verify the same process is attached and replacement cwd and command do not affect it.

## exit

- Role: atomic

Exit the application, verify its session disappears and reuse its name.

## integration

- Role: molecule
- Scope: coordinated
- Difficulty: 5 kyu
- Dependency: cli
- Children: path & terminal & omp & launcher_test & docs

Retain selection, session names and OMP command while switching the real launcher paths to rmux. Depends on the verified CLI; i3 PATH and interactive redraw are the main risks.

## path

- Role: atomic

Determine and implement a portable synced installation or explicit executable path that works from the actual i3 launcher environment.

## terminal

- Role: atomic

Preserve repository and worktree selection and existing session naming; use rmux up in the selected directory.

## omp

- Role: atomic

Preserve selection, session naming and the custom resume extension; use rmux up in the selected directory.

## launcher_test

- Role: atomic

Exercise the actual terminal and OMP entry points, including terminal close and OMP redraw.

## docs

- Role: atomic

The CLI usage message is the intended user-facing documentation; no separate usage document is required.

## kill_feature

- Role: material
- Scope: local
- Difficulty: 4 kyu
- Children: kill_change

Provide and verify named session termination as one material. Identifying the correct dtach session without affecting others is the main difficulty; no prerequisite feature work.

## kill_change

- Role: composite
- Scope: local
- Difficulty: 4 kyu
- Children: kill_session

One coherent CLI change including process identification, termination, help, and focused tests. The safe termination mechanism must be established before implementation.

## kill_session

- Role: molecule
- Scope: local
- Difficulty: 4 kyu
- Children: kill_method & kill_command & kill_usage & kill_live_test & kill_clients_test & kill_errors_test & kill_race_test

Implement and test rmux kill NAME as a single reviewable outcome. Investigate dtach behavior first; session lifecycle races and isolation are the main risks.

## kill_method

- Role: atomic

Track the dtach master identity when creating a session; pre-existing sessions without tracked identity must be ended manually. Investigate safe identity verification and application termination.

## kill_command

- Role: atomic

Add rmux kill NAME for sessions created with tracked identity. Stop the running application and report missing, untracked or unreachable sessions without affecting other sessions.

## kill_usage

- Role: atomic

Include kill NAME in the CLI usage message and help message.

## kill_live_test

- Role: atomic

Verify a detached application stops, the session is no longer listed, and its name can be reused.

## kill_clients_test

- Role: atomic

Verify attached clients disconnect and another live session remains unaffected.

## kill_errors_test

- Role: atomic

Verify invalid names, missing sessions and stale or unreachable sockets cannot terminate unrelated processes.

## kill_race_test

- Role: atomic

Exercise termination alongside session creation or attachment and verify it does not terminate a replacement or unrelated session.

## session_picker

- Role: material
- Scope: coordinated
- Difficulty: 4 kyu
- Children: picker_delivery

Allow independent persistent terminal and OMP sessions for the selected repository or worktree. Preserve existing clean session names, repository and worktree selection, and the OMP resume command.

## picker_delivery

- Role: composite
- Scope: coordinated
- Difficulty: 5 kyu
- Children: picker_work

One coherent change covering session selection in both dotfiles launchers and verification of both workflows. It depends on understanding the existing naming and creation behavior.

## picker_work

- Role: molecule
- Scope: coordinated
- Difficulty: 5 kyu
- Children: picker_naming & picker_list & picker_filter & picker_empty & picker_choice & picker_new & picker_verify

Update both launchers to list and filter live rmux sessions, create the first session directly, and otherwise offer attachment or creation of an additional uniquely suffixed session. Take care not to match another checkout merely because its name shares a prefix.

## picker_naming

- Role: atomic

Inspect how both launchers construct session names and create or attach to sessions.

## picker_list

- Role: atomic

Use rmux ls to obtain live session names for the launchers.

## picker_filter

- Role: atomic

Select the sessions belonging to the chosen repository or worktree and launcher type, including the existing clean name and additional suffixed names.

## picker_empty

- Role: atomic

When no matching sessions exist, create the session using the existing clean name and selected directory.

## picker_choice

- Role: atomic

When matching sessions exist, show them in a picker alongside a new-session option; attach to the chosen existing session and honor cancellation.

## picker_new

- Role: atomic

When new session is selected, create an independent session with a unique hash suffix, preserving the selected directory and launcher command.

## picker_verify

- Role: atomic

Exercise both launchers for clean creation, filtering, attachment, additional session creation, cancellation, and preserved worktree and OMP command behavior.

## interactive_mux

- Role: material
- Scope: coordinated
- Difficulty: 1 kyu
- Uncertainty: high
- Children: design_stage & runtime_stage & interface_stage & acceptance_stage

Deliver shared, persistent named sessions containing independent terminal buffers and a shared split-window view. Scope spans architecture, terminal rendering, input, lifecycle, existing CLI and launcher integration, and real interactive verification. Difficulty is high because terminal emulation, process lifetime, simultaneous attachments, and resize behavior interact. Architecture research precedes implementation; all child goals are required.

## design_stage

- Role: composite
- Scope: coordinated
- Difficulty: 2 kyu
- Children: architecture

Group the research into one coherent design decision and reviewable commit. Scope covers existing contracts, terminal technology and input; difficulty is high because the choice governs persistence and shared rendering. No prerequisites.

## architecture

- Role: molecule
- Scope: coordinated
- Difficulty: 2 kyu
- Uncertainty: high
- Children: inspect_contract & assess_engines & assess_dtach & inspect_st_keys & select_design

Investigate the current implementation, existing terminal technology and st input before choosing a design. This cross-cutting decision is difficult because buffers need terminal state, multiple views and persistent processes. It precedes implementation and must preserve existing interfaces unless a deliberate migration is needed.

## inspect_contract

- Role: atomic

The Bash CLI currently supports new, up, attach, ls and kill with --cwd and command forwarding. Tests cover session lifecycle, attach/detach, concurrent up and guarded process-tree kill. The i3 and OMP launchers rely on named sessions, checkout-specific cwd, four-hex additional-session suffixes, and the OMP custom-resume command; preserve these contracts during migration.

## assess_engines

- Role: atomic

Use a Rust session server/client with one PTY and vt100 terminal screen per buffer, a shared server-owned layout/focus model, and independently rendered attached clients. portable-pty provides PTYs and vt100 parses terminal streams into screen state; this directly models buffers referenced by multiple windows. tmux provides mature sessions and panes but its pane/window model does not directly allow one live buffer to appear in multiple layout cells, so using it unchanged would violate the contract.

## assess_dtach

- Role: atomic

dtach remains suitable only for the current one-command attach/detach session. Its one socket and one terminal stream do not provide independent PTYs/screens, shared split layout/focus, or the required minimum size across multiple views. Replace it as the interactive runtime rather than layering window behavior on the existing socket.

## inspect_st_keys

- Role: atomic

The checked-out and installed st binaries match. st maps Ctrl-Shift-H/L to CSI-u sequences; ordinary Ctrl letters are emitted as control bytes, so Ctrl-T/W/H/L need explicit distinct CSI-u mappings. Alt-letter combinations are emitted with an ESC prefix. The inspected i3 keybindings use Super-modified Alt or Ctrl combinations, not the proposed bare Alt controls. Verify end-to-end key delivery through live st/i3 during acceptance.

## select_design

- Role: atomic
- Dependency: inspect_contract & assess_engines & assess_dtach & inspect_st_keys

Decision: replace the dtach runtime with a Rust per-session server and attached terminal clients. The server owns session metadata, ordered buffer identities, shared window layout and focus, one PTY plus vt100 screen per buffer, and the attachment set. Each client renders the shared state into its local terminal; windows may reference the same buffer or none. Compute each buffer’s PTY dimensions as the minimum width and height among all windows showing it across all clients, then center that screen and fill unused window space with a patterned background. Use portable-pty for PTY management and vt100 for terminal parsing. Keep the public session CLI and launcher conventions; extend st key encoding for control chords that otherwise collapse to C0 bytes. This is chosen over unchanged tmux/dtach because neither existing process model directly supports a single live buffer referenced by multiple layout cells.

## runtime_stage

- Role: composite
- Scope: coordinated
- Difficulty: 2 kyu
- Children: session_runtime & buffer_runtime
- Dependency: design_stage

Group session lifecycle and terminal buffer work into one coherent runtime commit. Scope covers processes, clients, terminal state and compatibility; difficulty is high because lifecycle and terminal state interact. Depends on the architecture decision.

## session_runtime

- Role: molecule
- Scope: coordinated
- Difficulty: 2 kyu
- Children: preserve_cli & session_server & client_attach & client_detach & attachment_tracking & session_cleanup & launcher_migration
- Dependency: architecture

Provide the persistent session and attachment lifecycle using the chosen architecture while preserving the existing CLI and launcher behavior. This spans process management and clients; difficulty lies in safe lifetime and concurrent attachments. Depends on the architecture decision.

## preserve_cli

- Role: atomic
- Dependency: architecture

Preserve name handling, creation command and working directory semantics, and safe session termination.

## session_server

- Role: atomic
- Dependency: architecture

## client_attach

- Role: atomic
- Dependency: architecture

## client_detach

- Role: atomic
- Dependency: architecture

## attachment_tracking

- Role: atomic
- Dependency: architecture

## session_cleanup

- Role: atomic
- Dependency: architecture

## launcher_migration

- Role: atomic
- Dependency: architecture

Preserve project and worktree selection, multiple named sessions and the OMP resume command.

## buffer_runtime

- Role: molecule
- Scope: coordinated
- Difficulty: 2 kyu
- Children: buffer_process & terminal_state & hidden_buffer & buffer_order & buffer_close & buffer_exit & final_buffer
- Dependency: architecture & session_runtime

Manage running terminal programs independently of their display windows. Scope includes terminal state and process lifecycle; difficulty is preserving interactive application behavior while buffers are hidden or multiply displayed. Depends on the architecture decision and persistent session runtime.

## buffer_process

- Role: atomic
- Dependency: architecture & session_runtime

## terminal_state

- Role: atomic
- Dependency: architecture & session_runtime

## hidden_buffer

- Role: atomic
- Dependency: architecture & session_runtime

## buffer_order

- Role: atomic
- Dependency: architecture & session_runtime

## buffer_close

- Role: atomic
- Dependency: architecture & session_runtime

## buffer_exit

- Role: atomic
- Dependency: architecture & session_runtime

## final_buffer

- Role: atomic
- Dependency: architecture & session_runtime

## interface_stage

- Role: composite
- Scope: coordinated
- Difficulty: 2 kyu
- Children: window_layout & presentation & input_controls & buffer_rename & equalize_windows
- Dependency: runtime_stage

Group shared window layout, rendering, and keyboard controls into one interactive interface. This spans layout state, attached-client rendering, and input; the existing window behavior is complete, with buffer naming and window equalization added as cross-cutting interface features. Depends on the runtime.

## window_layout

- Role: molecule
- Scope: coordinated
- Difficulty: 2 kyu
- Children: initial_window & vertical_split & horizontal_split & close_window & last_window & focus_window & move_window & resize_window & switch_buffer & reorder_buffer & new_buffer_view & closed_buffer_views
- Dependency: buffer_runtime

Represent the split layout and focused rmux window once per session. A window displays one buffer, while a buffer may appear in multiple windows or none. Scope covers layout and navigation; difficulty is preserving shared state and handling window and buffer lifecycle transitions. Depends on terminal buffers.

## initial_window

- Role: atomic
- Dependency: buffer_runtime

## vertical_split

- Role: atomic
- Dependency: buffer_runtime

## horizontal_split

- Role: atomic
- Dependency: buffer_runtime

## close_window

- Role: atomic
- Dependency: buffer_runtime

## last_window

- Role: atomic
- Dependency: buffer_runtime

## focus_window

- Role: atomic
- Dependency: buffer_runtime

## move_window

- Role: atomic
- Dependency: buffer_runtime

## resize_window

- Role: atomic
- Dependency: buffer_runtime

## switch_buffer

- Role: atomic
- Dependency: buffer_runtime

## reorder_buffer

- Role: atomic
- Dependency: buffer_runtime

## new_buffer_view

- Role: atomic
- Dependency: buffer_runtime

## closed_buffer_views

- Role: atomic
- Dependency: buffer_runtime

Select a remaining buffer when available; closing the final buffer ends the session.

## presentation

- Role: molecule
- Scope: coordinated
- Difficulty: 2 kyu
- Children: render_layout & buffer_line & buffer_geometry & notify_resize & centered_view & background_pattern & geometry_updates & shared_redraw
- Dependency: buffer_runtime & window_layout & session_runtime

Draw the shared layout, buffer line and terminal states in each attached terminal, including different sizes. This is difficult because a buffer has one application size despite multiple views and clients. Depends on buffers, shared windows and attachment tracking.

## render_layout

- Role: atomic
- Dependency: buffer_runtime & window_layout & session_runtime

## buffer_line

- Role: atomic
- Dependency: buffer_runtime & window_layout & session_runtime

## buffer_geometry

- Role: atomic
- Dependency: buffer_runtime & window_layout & session_runtime

## notify_resize

- Role: atomic
- Dependency: buffer_runtime & window_layout & session_runtime

## centered_view

- Role: atomic
- Dependency: buffer_runtime & window_layout & session_runtime

## background_pattern

- Role: atomic
- Dependency: buffer_runtime & window_layout & session_runtime

## geometry_updates

- Role: atomic
- Dependency: buffer_runtime & window_layout & session_runtime

## shared_redraw

- Role: atomic
- Dependency: buffer_runtime & window_layout & session_runtime

## input_controls

- Role: molecule
- Scope: coordinated
- Difficulty: 3 kyu
- Children: key_protocol & buffer_keys & buffer_navigation_keys & buffer_move_keys & window_split_keys & window_close_key & window_focus_keys & window_move_keys & window_size_keys & st_integration
- Dependency: architecture & buffer_runtime & window_layout

Translate the agreed shortcuts into shared session actions while allowing terminal applications to receive other input. Difficulty lies in terminal encoding, existing application bindings and st or i3 interception. Depends on architecture, buffers and windows; exact encoding and conflict policy require investigation.

## key_protocol

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## buffer_keys

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## buffer_navigation_keys

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## buffer_move_keys

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## window_split_keys

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## window_close_key

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## window_focus_keys

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## window_move_keys

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## window_size_keys

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

## st_integration

- Role: atomic
- Dependency: architecture & buffer_runtime & window_layout

Preserve existing st functionality and use the synced configuration if a change is needed.

## acceptance_stage

- Role: composite
- Scope: coordinated
- Difficulty: 2 kyu
- Children: verification & usage_docs
- Dependency: interface_stage

Group end-to-end acceptance and usage documentation into one coherent delivery commit. Scope includes real clients, launchers and user-facing documentation; difficulty is high because interactive behavior needs verification across terminals. Depends on the interactive interface.

## verification

- Role: molecule
- Scope: coordinated
- Difficulty: 2 kyu
- Children: verify_cli & verify_launchers & verify_detach & verify_hidden & verify_splits & verify_independent_switch & verify_window_close & verify_buffer_close & verify_buffer_line & verify_window_controls & verify_shared_clients & verify_shared_size & verify_centering & verify_resize_events & verify_keys & verify_application_input & verify_st_patch & verify_buffer_rename & verify_equalize & verify_atuin & verify_pane_color
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

Automated runtime checks pass, and the user confirmed Alt-V and Alt-B in st without the rmux-specific patch. Complete real desktop verification of all key combinations, SGR 59 warning behavior, buffer-line background, equalization and the i3 launcher path. The former rmux-specific st keybinding patch is incompatible with the current ordinary-key decoder.

## verify_cli

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_launchers

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_detach

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_hidden

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_splits

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_independent_switch

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_window_close

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_buffer_close

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_buffer_line

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_window_controls

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_shared_clients

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_shared_size

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_centering

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_resize_events

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## verify_keys

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls & verify_st_patch

The user verified the real Super+Ctrl+T i3 launcher and two attachments to one session. They tested window splits, focus, movement, resizing, buffer navigation, reordering and renaming, Alt+= equalization, final-window protection, final-buffer session termination, and reattachment after closing both terminal windows. Ctrl-Shift-H/L, Alt-H/J/K/L, Alt-Shift-H/J/K/L, and Alt-Ctrl-H/J/K/L were confirmed individually. Automated PTY and lifecycle tests cover ordinary input and shared sessions.

## verify_application_input

- Role: atomic
- Dependency: session_runtime & buffer_runtime & window_layout & presentation & input_controls

## usage_docs

- Role: molecule
- Scope: coordinated
- Difficulty: 5 kyu
- Children: document_terms & document_controls & document_lifecycle & document_views
- Dependency: verification

Document the delivered session, buffer and rmux window model, controls, lifetime and attachment behavior. Scope is user-facing guidance; difficulty depends on the final implementation and verified shortcuts. Depends on complete interactive verification.

## document_terms

- Role: atomic
- Dependency: verification

## document_controls

- Role: atomic
- Dependency: verification & buffer_rename & equalize_windows

Document all supported keyboard controls and application conflicts, including Ctrl-R buffer renaming and Alt+= window equalization after those features are implemented. State any verified physical-key fallback required by st.

## document_lifecycle

- Role: atomic
- Dependency: verification

## document_views

- Role: atomic
- Dependency: verification

## verify_st_patch

- Role: atomic
- Scope: coordinated
- Difficulty: 5 kyu
- Uncertainty: high
- Dependency: input_controls & st_integration

The user verified the installed runtime works with their unchanged st and no rmux-specific keybinding patch. Alt-V/B, Ctrl-Shift-H/L, Alt-H/J/K/L, Alt-Shift-H/J/K/L, and Alt-Ctrl-H/J/K/L work; the user also confirmed the SGR 59 warning is gone and the pane colors are correct.

## buffer_rename

- Role: molecule
- Scope: coordinated
- Difficulty: 5 kyu
- Uncertainty: high
- Dependency: buffer_runtime & presentation & input_controls
- Children: buffer_name_state & rename_buffer_key

Keep stable numeric IDs separate from user-visible labels. Initialize each name from its numeric ID, share names through layout snapshots, and show the names in the buffer line across attachments and windows. Ctrl-R opens a prefilled rename prompt for the focused buffer; Enter commits, Escape cancels, and Ctrl-U clears the name so Enter restores the numeric label.

## buffer_name_state

- Role: atomic
- Scope: coordinated
- Difficulty: 5 kyu
- Dependency: buffer_runtime & presentation

Store a user-visible name alongside each stable buffer ID, initialize it from the current numeric label, include it in shared layout state, and render the same name in every attached client.

## rename_buffer_key

- Role: atomic
- Scope: coordinated
- Difficulty: 5 kyu
- Dependency: buffer_name_state & input_controls

Ctrl-R opens a local prompt for the focused buffer, prefilled with its current display name. Enter commits the UTF-8 name and broadcasts it to all attached clients; Escape cancels without changing the old name. Ctrl-U clears the field, so committing an empty name restores the numeric buffer ID. Prompt input must never reach the buffer's terminal program.

## verify_buffer_rename

- Role: atomic
- Scope: coordinated
- Difficulty: 5 kyu
- Dependency: buffer_rename

Verify through automated tests that Ctrl-R enters the local prompt, Enter commits a UTF-8 label, Escape cancels, and prompt text never reaches the terminal program. Check that the shared label is visible to a second attachment, remains associated with the same buffer ID and order, persists when switching away and back, and is used when multiple rmux windows show that buffer. Confirm clearing with Ctrl-U restores the numeric label.

## equalize_windows

- Role: molecule
- Scope: coordinated
- Difficulty: 5 kyu
- Uncertainty: high
- Dependency: window_layout & input_controls
- Children: equalize_layout & equalize_key

Provide a shared layout command that makes visible rmux windows approximately equal in terminal-cell area while preserving the split tree, buffers, and focus. This molecule covers the ratio algorithm, Alt+= CSI-u command path, and st key mapping. Physical key delivery remains tracked by verify_equalize and verify_st_patch.

## equalize_layout

- Role: atomic
- Scope: local
- Difficulty: 5 kyu
- Dependency: window_layout

Rebalance ratios recursively from the number of window leaves in each child subtree. Rounded percentage ratios approximate equal pane area; geometry deterministically assigns integer remainder cells. Changing ratios alone preserves the split tree, window and buffer IDs, and focus.

## equalize_key

- Role: atomic
- Scope: coordinated
- Difficulty: 5 kyu
- Dependency: equalize_layout & input_controls & st_integration

Decode CSI-u Alt+= (ESC[61;3u) as the equalize command, leave unmatched input unchanged, dispatch it through the shared session layout, and add the matching XK_equal/Mod1Mask mapping to st-rmux-keybindings.diff.

## verify_equalize

- Role: atomic
- Scope: coordinated
- Difficulty: 5 kyu
- Dependency: equalize_windows

Automated geometry and two-client integration tests confirm ordinary Alt+= balances pane areas and broadcasts the shared layout while preserving axes, IDs, and focus. The user tested Alt+= using Alt+Shift+0 with two terminal windows attached to the same session and reported that the two rmux windows were equalized correctly.

## verify_atuin

- Role: atomic
- Scope: coordinated

Atuin interactive history search previously timed out because the rmux buffer terminal did not answer CSI 6 n. The runtime now responds from its per-buffer terminal parser. Automated tests cover fragmented requests and cursor replies with and without attached clients. The user tested the installed fix in a new session and reported that Atuin appears to work.

## verify_pane_color

- Role: atomic
- Scope: local

The user reported that the first uncolored text appeared gray after the buffer line. Fixed a renderer ordering bug that reset the first pane cell after setting its colors. A color-enabled regression test verifies the first colored cell and subsequent default-colored cell. The user opened a fresh session through Super+Ctrl+T, cleared the screen, and confirmed the color was correct.

## version_flags

Implement --version and -v so either flag prints the installed rmux version. Use Semantic Versioning (https://semver.org/): increment MAJOR for incompatible changes, MINOR for backward-compatible functionality, and PATCH for backward-compatible bug fixes. During initial development (0.y.z), the public interface is not considered stable.

## configuration

- Scope: coordinated
- Children: config_loading & config_keys & config_buffer_colors & config_dividers & config_margin & config_resize & config_docs
- Difficulty: 4 kyu
- Uncertainty: high
- Role: material

Add user configuration in ~/.config/rmux/config.toml, retaining current behavior when settings are omitted. Cover existing rmux keybindings, buffer-line colors, current single-divider characters and colors, unused-space fill and color, and window resize step. Keep configuration for future per-window borders best effort: the border design may change when window_borders is implemented. Buffer-line markers are tracked by buffer_line_markers; Vim-mode configuration belongs with vim_mode when implemented. Define exact syntax and error behavior during implementation.

Grouped plan:
- Material: develop the complete configuration feature in a worktree.
- Composite 1: configuration and controls. Review the TOML loading and defaults as one molecule, then keybindings and resize-step behavior as a second molecule; commit them together.
- Composite 2: configuration appearance and documentation. Review buffer-line colors as one molecule, divider and unused-space styling as a second molecule, and the documentation after those settings work; commit them together.
- Keep the feature groups and their atomic steps as the completion checklist. The grouped plan can change as implementation reveals better boundaries.

Load configuration at startup or attachment. Hot reload is outside the scope of this task and will not be implemented.

## config_loading

- Role: molecule
- Scope: coordinated
- Difficulty: 6 kyu
- Children: config_schema & config_path & config_read & config_parse & config_validate & config_load_tests & config_startup & config_start_tests

Load ~/.config/rmux/config.toml and apply existing defaults for omitted settings; report invalid configuration clearly.

## config_keys

- Role: molecule
- Dependency: config_loading
- Scope: coordinated
- Difficulty: 4 kyu
- Children: keys_syntax & keys_parse & keys_apply & keys_tests

Apply configured shortcuts to every existing rmux command and retain the existing defaults for omitted bindings.

## config_buffer_colors

- Role: molecule
- Dependency: config_loading
- Scope: coordinated
- Difficulty: 5 kyu
- Children: buffer_color_states & buffer_color_render & buffer_color_tests

Configure the line background and foreground and background colors for focused, shown-but-not-focused, and unshown buffers.

## config_dividers

- Role: molecule
- Dependency: config_loading
- Scope: local
- Difficulty: 6 kyu
- Children: divider_style & divider_tests

Configure the characters and color of the existing single dividers, preserving their current appearance by default; future border settings are best effort.

## config_margin

- Role: molecule
- Dependency: config_loading
- Scope: local
- Difficulty: 6 kyu
- Children: margin_style & margin_tests

Configure the fill and color around buffer views smaller than their windows.

## config_resize

- Role: molecule
- Dependency: config_loading
- Scope: coordinated
- Difficulty: 5 kyu
- Children: resize_apply & resize_tests

Apply a configured resize step to window resizing while retaining the current default.

## config_docs

- Role: atomic
- Dependency: config_keys & config_buffer_colors & config_dividers & config_margin & config_resize

Document the supported TOML settings, defaults, and keybinding syntax after the behavior works.

## config_schema

- Role: atomic

Represent supported TOML settings with defaults matching existing behavior.

## config_path

- Role: atomic

Resolve the agreed ~/.config/rmux/config.toml path without requiring the file to exist.

## config_read

- Role: atomic
- Dependency: config_path

Read the file when present and report file access errors.

## config_parse

- Role: atomic
- Dependency: config_schema & config_read

Parse the file into the configuration types and report malformed TOML.

## config_validate

- Role: atomic
- Dependency: config_parse

Reject invalid settings with actionable errors while retaining defaults for omitted values.

## config_load_tests

- Role: atomic
- Dependency: config_validate

Cover missing and partial files, valid TOML, malformed TOML, invalid settings, and read errors.

## keys_syntax

- Role: atomic
- Dependency: config_loading

Choose a readable representation for existing commands and terminal key sequences, including conflicting bindings.

## keys_parse

- Role: atomic
- Dependency: keys_syntax

Convert configured keys to input sequences and reject invalid or conflicting bindings.

## keys_apply

- Role: atomic
- Dependency: keys_parse

Use configured bindings for existing commands while keeping omitted bindings at their defaults.

## keys_tests

- Role: atomic
- Dependency: keys_apply

Verify remapping, defaults, conflicts, and ordinary input passthrough through the client.

## buffer_color_states

- Role: atomic
- Dependency: config_loading

Distinguish focused, shown-but-not-focused, and unshown buffers in the buffer line.

## buffer_color_render

- Role: atomic
- Dependency: buffer_color_states

Apply the configured background and each state foreground and background.

## buffer_color_tests

- Role: atomic
- Dependency: buffer_color_render

Verify state-specific colors and defaults with multiple visible and hidden buffers.

## divider_style

- Role: atomic
- Dependency: config_loading

Apply configured characters and color to existing horizontal and vertical dividers without changing default geometry.

## divider_tests

- Role: atomic
- Dependency: divider_style

Verify custom divider appearance and existing defaults.

## margin_style

- Role: atomic
- Dependency: config_loading

Apply the configured fill and color to unused space around smaller buffer views.

## margin_tests

- Role: atomic
- Dependency: margin_style

Verify custom and default fill in differently sized views of the same buffer.

## resize_apply

- Role: atomic
- Dependency: config_loading

Use the configured step for window resizing while retaining the current default.

## resize_tests

- Role: atomic
- Dependency: resize_apply

Verify configured and default resize steps and boundary behavior.

## config_startup

- Role: atomic
- Dependency: config_validate

Integrate configuration loading into client and session-server startup and provide the resulting settings to their consumers. Define which settings belong to an attachment and which belong to a session.

## config_start_tests

- Role: atomic
- Dependency: config_startup

Verify settings reach the client and server, including attachment to an existing session and changes to the configuration between launches.

## buffer_line_markers

- Dependency: configuration

Allow independent prefix and suffix characters for buffer names in each buffer-line state: focused, shown in another rmux window, and unshown. Configure them alongside the corresponding colors in ~/.config/rmux/config.toml. Support ASCII and Unicode characters, including Nerd Font glyphs, and account for their terminal display widths. Preserve the current focused-buffer brackets and other existing appearance as defaults.

## omp_paste_submission

When OMP runs inside rmux, pasting multiline text with Ctrl+Shift+V can submit lines as separate messages without pressing Enter, causing the agent to start before receiving the full input. The reported example was a paragraph with a trailing newline. Outside rmux, the same paste remains in the chat input without submission. Reproduce the difference, identify and fix the underlying input-handling issue, and verify that multiline and trailing-newline pastes remain intact without being sent until explicitly submitted.

## rename_word_editing

In the buffer rename prompt, make Ctrl-Backspace delete the preceding word without leaving rename mode. Support Ctrl-Left and Ctrl-Right to move the cursor between words while editing the name.

## omp_resize_crash

Fix the rmux client disconnect reported during repeated st floating-mode toggles and when resuming a busy OMP session. Diagnostic logs captured the server disconnecting the client because its 64-message output queue filled; the rmux server and OMP process remained alive. Prevent output bursts from disconnecting clients while preserving terminal output and session responsiveness, and verify resizing and busy OMP sessions.

The user previously observed the problem with one OMP session and more often with two. Missing cursor-position replies were an earlier unconfirmed hypothesis.

