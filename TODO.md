# TODOs

- [ ] TODO: Select or create sessions in both launchers [session_picker](#session_picker)
  - [ ] TODO: Deliver session selection for both launchers [picker_delivery](#picker_delivery)
    - [ ] TODO: Implement and verify session selection [picker_work](#picker_work)
      - [ ] TODO: Identify current session naming and creation [picker_naming](#picker_naming)
      - [ ] TODO: List existing rmux sessions [picker_list](#picker_list)
      - [ ] TODO: Filter sessions for the selected checkout [picker_filter](#picker_filter)
      - [ ] TODO: Create a clean session when none exists [picker_empty](#picker_empty)
      - [ ] TODO: Offer existing sessions and new session [picker_choice](#picker_choice)
      - [ ] TODO: Create an additional suffixed session [picker_new](#picker_new)
      - [ ] TODO: Verify both launcher workflows [picker_verify](#picker_verify)



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
