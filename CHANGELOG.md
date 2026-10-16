# Changelog

What changed in each version of SuperCI. The dashboard shows the entries your control plane does not have yet.

## 0.19.6 — unreleased
- Runners is one list in order: AWS spot, AWS on-demand, Cloudflare and Modal are alike rows, each with jobs at once, a monthly budget and where it starts machines (AWS's on-demand machines now have regions of their own, like its spot ones). Any row can be taken out of the order and put back, its settings kept; `superci runners order` takes out the ones it does not name. The accounts (AWS's own network, Cloudflare's image, removing one) are under Clouds.
- Runs from a computer run a job that calls a workflow of the repository (`uses: ./.github/workflows/x.yml`) as GitHub does: its jobs (`caller / job`), with the inputs and secrets the caller gives (`with:`, `secrets: inherit` or named), and its outputs for the jobs that need the caller. (One of another repository is not run from a computer yet.)
- Runs from a computer run steps side by side as GitHub does: `background: true`, `wait`, `wait-all`, `cancel` and `parallel` groups, refused where GitHub refuses them.
- A GitHub job whose steps ran side by side shows each step's own lines on its page, from its run's logs once the run has ended (until then its lines are in the order they came, and the page says so).
- On AWS and Modal control planes, a job's Raw log opens GitHub's own link again, whatever its size.
- With several workspaces on the computer, dragging Runners into a new order saves it, instead of putting it back as it was.
- With several workspaces on the computer, `superci login cloudflare` and `superci login modal --workspace=ID` open that cloud's sign-in in the workspace named, and a page that leads to a part of another one (Control plane's AWS account, its Permissions) stays in the workspace it was opened in.
- `superci login aws --workspace=ID` with several workspaces on the computer: Continue to AWS goes on to AWS, instead of a page asking which workspace.
- A matrix made by an expression (`${{ fromJSON(needs.setup.outputs.matrix) }}`) is read once the jobs it needs have ended, as on GitHub.
- A job's `timeout-minutes` ends it once it has run that long, and its `continue-on-error` lets the run go on when it fails (the jobs that need it see a success, as on GitHub).
- Runs from a computer read YAML anchors and aliases in workflow files, as GitHub Actions does; a merge key (`<<`) is refused, as GitHub Actions refuses it.
- `superci runs retrieve ID --tests` (and `superci jobs retrieve ID --tests`): each failed job's tests, read from its whole log: the tests that failed, each with its file and error, how many passed, failed and were skipped, and where its artifacts are. It reads what Vitest, Jest, Playwright, pytest, cargo and Go print. For a coding agent: what failed, without reading the log.
- A large log's page is quick: a step's lines are drawn when it is opened, 2,000 at a time (Show more), search looks through the log itself and shows the lines that hold the words, and a finished job's log is kept for its next opens.
- An AWS sign-in that AWS does not finish (seen when the account is changed on its page midway) says so plainly, with the way back to the dashboard to sign in again, instead of AWS's raw error.
- When the control plane's cloud sign-in has ended (AWS ends one after 12 hours), the update card's button signs in first and comes back, instead of an Update that stops on "Sign in with AWS first".
- Runs first in the dashboard: Overview lists the latest runs, and its first figure counts runs, with their jobs beside it (from a control plane on 0.19.6, which also counts the runs of the jobs it kept from before, once, in its upkeep; jobs until it is updated). The sidebar's Jobs is Runs, its Runs tab first.
- A job sent on-demand by the AWS account's spot limit says when machines SuperCI had just ended still counted ("machines AWS was still ending count too"): AWS counts a machine for a minute or two after its job ends.

## 0.19.5 — 2026-10-15
- A job on AWS says the CPUs of the machine it got: one that asked for 128 on c8a (which has no 128-CPU size) got a 192-CPU c8a.48xlarge, and said 128.
- AWS machines above 64 CPUs are asked in each family's own sizes: the 128-CPU size where a family has it (c7a, m7a, r7a, and the families with a local disk), and no size a family lacks (c7g, m7g and r7g end at 64; c6id, m6id and r6id at 128).

## 0.19.4 — 2026-10-15
- A finished job's page shows its whole log, whatever its size: a GitHub job's from GitHub, a run from a computer's from the bucket. It read only the last 1 MB, so a large log's first steps looked empty. A log that is still cut (a running job's, or one from a control plane older than 0.19.1) says so.
- `superci jobs retrieve ID --out=FILE` says when the control plane gave only the end of a log, instead of keeping that as the whole.
- `superci runs create --quiet` prints how each step went and the last lines of one that failed, not every line: for a coding agent, whose every line read costs. `--detach` says the run's id and ends; `superci runs wait ID` follows it. Every run now ends with its steps' results and times, its machine and what it cost.
- A run from a computer's page in the dashboard lists its artifacts, each a download.
- A job's whole log, whatever its size: `superci jobs retrieve ID --out=FILE`, and the dashboard's Raw log, take a run from a computer's from its pieces in the control plane's bucket, and a GitHub job's from GitHub (4 MB at most before).
- Windows machines say how their start went (boot, start-up script, the runner), try the runner's addresses at GitHub before starting it, and send its lines as they are written, as Linux machines do.
- AWS's limit on CPUs at once is asked for more of from one button, with spot and on-demand apart, each with its limit now.
- `superci projects list` without GitLab says so; amounts with nothing spent say 0, not -0.
- A run from a computer says why a job failed when no machine began it ("Cloudflare did not start a machine for it, three times"), and a job ended for running past its longest time says so, there, in `superci jobs retrieve` and in the dashboard.
- A job that ran and failed is no longer said to have not started, on the Jobs page and on its page.
- Saving the default machine returns to Workflows, where it is set. `superci runners update CLOUD --enabled=false` is refused for a provider other than aws-on-demand (it did nothing): another is removed with `superci runners delete CLOUD`.

## 0.19.3 — 2026-10-15
- `superci runs create` no longer goes quiet: after a burst of lines GitHub's runner did not send as fast as they came, 0.19.0 waited for the step to end before it printed more of it, minutes for a long step. What was dropped is now filled from what the machine sends of the step's own log every two seconds, and the lines go on as they come; the few the runner still holds come when the step ends, said as such.
- A runner's lines are kept as they come, with nothing else read or written each time (each post of a hundred lines rewrote the job's whole record).
- `superci jobs retrieve` shows a step still going from what its machine sent of the step's log, not only the lines the runner sent as it went.
- The ids of runs from a computer are below 2^53, so GitHub's actions print them whole ("Artifact download URL" had the run's id rounded).

## 0.19.2 — 2026-10-15
- `superci runs create` and `superci caches delete` go on when SuperCI's cloud sign-in has ended, or another account is signed in: they need only this computer's key with the control plane it is kept for, as looking at jobs, runs, usage and artifacts already did. (AWS ends its sign-in after twelve hours, and when you sign out of the AWS console to sign in to another account.) Changing a control plane's settings still needs a sign-in to the cloud account it runs in, and says so.

## 0.19.1 — 2026-10-15
- A GitHub job's page shows its lines while it runs. GitHub gives a job's log only once it has ended; its machine now sends what the runner writes of each step as it goes (every two seconds, in the pieces the runner writes them), kept in the control plane's bucket until GitHub's own log is there. A step that writes little shows its lines when it ends.
- Runs from a computer send only what changed since the working tree this computer last sent whole (kept in the control plane's bucket as a base): a few files' worth instead of the whole tree, for an agent that runs the tests after each edit. The machine puts the base in place, then the changes, and removes what was removed. A base gone (the bucket keeps it up to a month), or changes of half the tree or more, and the whole tree is sent again.
- A job's log is given up to 4 MB (1 MB before) by `superci jobs retrieve --bytes` and the dashboard, and a GitHub job's Raw log comes whole, from GitHub itself, whatever its size.
- A control plane on Modal keeps each kind of record in a Dict of its own, so looking up one kind no longer reads every record; what it kept before is moved over by itself.

## 0.19.0 — 2026-10-15
- `superci runs create` prints every line of a run's jobs. GitHub's runner sends its lines as it goes only as fast as it can, holding 1,024 at most and dropping the rest, so a job that printed a lot (an e2e suite's 5,000 lines) was cut short though it passed. Each step's whole log, which the runner sends when the step ends, is now kept (in the control plane's bucket, where there is one) and what was dropped is printed from it, in order. The dashboard's log of such a job is the whole one too.
- The control plane keeps its records apart by kind (on AWS, a DynamoDB partition for each), so one kind's traffic never slows another's; what an earlier version kept is moved over by itself, a little at each minute's sweep, with nothing to do.
- Starting a machine, the minute's sweep and the dashboard read only the jobs still going and each day's totals, not every job ever kept, so they stay as fast with a year of history as with a day's. Spend, limits and usage come from each day's totals.
- Records end by themselves: a job's 90 days after it ends (about two months before), its lines and logs after two days, a machine's start record with its job, cache entries with their pieces in the bucket.
- A job's log in the dashboard is in steps, as GitHub shows one: how each step went and how long it took, its lines numbered and in colour, a step's errors and warnings marked and counted, the step running (or the one that failed) open. A GitLab job's steps are its sections, by GitLab's own headings. While the job runs the log reads on by itself and adds what is new where you are, rather than drawing the page anew (which put the log back at its top). Search it, show each line's time, or take the raw log.
- A runner of GitHub's whose first connection GitHub refused waited 30 s before it tried again (GitHub's runner waits that long), so its job started half a minute late. The machine now tries the runner's addresses at GitHub first, every second, beside reading the runner's files, and starts the runner once they answer: a refusal costs as long as it lasts. A job's start says so ("GitHub refused the first connection; reachable after 3 s"). The start-up script also leaves out its explaining lines, so it stays well within what AWS takes.
- `superci status --json` says how the control plane's store stands (`store`: its jobs indexed, nothing left in an earlier layout).

## 0.18.2 — 2026-10-14
- Overview's figures cover the last 24 hours, 7 days or 30 days (a switch on the card), counted by the control plane over every job it keeps, not from the latest fifty: jobs, machine minutes beside the minutes GitHub or GitLab would bill (each job rounded up to a minute), the wait for a machine, and what they cost beside what the code host's own runners of the same sizes would have charged, with what that saved. `superci usage retrieve --period=day|week|month` says the same. The page waits in the same shape while it loads.
- A run from a computer with a working tree of many MB failed before it started: the tree went into the control plane's DynamoDB table, which writes only so much a second, and DynamoDB refused it ("ThrottlingException"). The tree now goes into the control plane's bucket in one piece, as a build service takes its source, and its machine fetches it from there; a tree may be 1 GB packed (150 MB before). A control plane with no bucket takes it as before.
- The control plane on AWS asks DynamoDB again when it says it is too busy, waiting a little longer each time, as AWS's own tools do, rather than fail at once.

## 0.18.1 — 2026-10-14
- What a run from a computer's jobs keep with `upload-artifact` can be fetched to the computer: `superci artifacts list --run=ID` names them, `superci artifacts retrieve NAME --run=ID [--out=DIR]` unpacks one, as `download-artifact` would. With a key that only reads too.
- A key for coding agents has no limits of its own: its jobs keep to the control plane's, as every job does (the largest machine, what may be spent in a month and a day). `superci keys create agent --runs` takes no `--max-cpu` or `--daily-usd`; a key made by 0.18.0 is no longer taken, so make it again.
- `superci runs create` against a control plane older than 0.18.0 says to update it, rather than that the control plane has nothing there.

## 0.18.0 — 2026-10-14
- GitHub's cache, kept in your own AWS account: each region your machines run in gets a bucket of the control plane's, and a free S3 endpoint in its network, so a cache never leaves your account and never goes over the internet. Nothing in your workflows changes: `actions/cache`, the setup actions' caches, Docker's `type=gha` and the rest reach it as they reach GitHub's. On a 16-CPU machine a 4 GB cache saved in 10 s and was back in 11 s (GitHub's own: 12 s and 21 s).
- Each machine runs `superci-cache`, which holds no keys: for every lookup and save it asks the control plane with the job's own token, and the control plane checks it against GitHub's keys and signs links to the bucket that each do one thing. A job reads and writes what GitHub would let it: a pull request reads its own branch's caches, then its base's and the default branch's, and writes only its own.
- On unless turned off, and what it keeps is yours to set (Settings → Cache, or `superci caches update --enabled=false --size-gb=25 --days=7`): each repository 25 GB, the least recently used going first, and an entry unused for 7 days goes. An entry saved 30 days ago is saved anew whatever its use, and the bucket removes anything older than 32 days by itself.
- `superci caches retrieve` says what each repository keeps, `superci caches list --repo=OWNER/NAME` its entries (key, branch, size, last use), and `superci caches delete --repo=OWNER/NAME [KEY] --confirm` takes them out; Settings → Cache clears a repository with a button.
- Caches saved at GitHub before are used until they age: a lookup not found in your bucket is asked of GitHub. When the control plane cannot be reached, jobs use GitHub's cache as before. Artifacts of GitHub's jobs stay at GitHub.
- Turbo's, Nx's and Bazel's remote caches go to the same bucket, with nothing to set: each machine tells them where (Turbo and Nx in the runner's environment, Bazel in its system-wide settings), and a workflow's own settings still come first (a Vercel token goes on to Vercel). They carry no GitHub token, so what they may read and write is what GitHub said of the job: its branch's to write, then the default branch's to read; a branch never writes where the default branch reads.
- Windows machines on AWS use it too, and GitLab jobs on AWS keep GitLab's own cache there (GitLab's runner is pointed at it, as S3), by project and by the branch GitLab said the job runs for.
- The control plane's role may read, write and remove what is in its own buckets (`CacheBucket`); the bucket and the endpoint are made by the dashboard with your sign-in, beside each region's network. Machines on Cloudflare and Modal keep using GitHub's cache.
- Runs from a computer: `superci runs create --workflow=.github/workflows/ci.yml` runs a workflow's jobs as GitHub would run them, each on a machine as any job of your control plane's, with the working tree as it is on your computer and nothing pushed. What git tracks and the new files it does not ignore go along (an ignored `.env` stays behind), through your control plane: no new resource or permission in your cloud. It is kept there only until the run's jobs have fetched it. GitHub's own runner runs the steps, as in CI: actions, service containers, expressions, step outputs.
- What GitHub would give such a job and cannot here is given on the command line: `--secret=NAME=VALUE` (hidden in the job's lines as GitHub hides secrets, and not kept once the job has ended) and `--var=NAME=VALUE`; `--event=pull_request` when the workflow does something else for one. There is no `GITHUB_TOKEN` and no OIDC.
- Their jobs use the cache as CI's do: they read the caches of their branch and the default branch (what GitHub's jobs saved), and write a scope of their own, so a run from a computer never changes what CI finds. Their artifacts are kept in the same bucket for as long as the run (a day): `upload-artifact` in one job and `download-artifact` in the next work as on GitHub.
- As GitHub runs them: each combination of a matrix is a job of its own, on the machine its `runs-on` says; a job starts once the jobs it `needs` have ended, runs if its `if` holds (by default, if they all succeeded), and knows their results and outputs. `--job=NAME` runs that job and those it needs.
- The command waits for the run and prints its jobs' lines; it fails when the run does. Its jobs are listed with the others (`superci jobs list`, Runs), with their machines and their cost.
- A key for coding agents: `superci keys create agent --runs` makes a key that starts such runs and changes nothing else, within limits (`--max-cpu=16` CPUs a job, `--daily-usd=10` of machines a day, three runs at a time). With it and your control plane's address, `superci runs create` works with no sign-in.
- Each run (a workflow run on GitHub, a pipeline on GitLab, a run from a computer) has a page too: how it stands, how long it took, what its machines cost, the job it waited for last, and its jobs, each opening its own page; GitHub's or GitLab's page of it is a button there, with its logo. A job's page opens its run's.
- A GitLab job that ran and failed is said as failed by itself, as a GitHub job is, not as a job SuperCI could not start. Its time is GitLab's own, from when it began to when it ended (a job a sweep first saw could show 0 s).
- Each job has a page in the dashboard: how it stands, its machine and cost, how its start went step by step, and its log, looked at again every few seconds while it runs. Its page at GitHub or GitLab is a button on it; a run from a computer, which has no such page, is followed there.
- A GitLab job whose event GitLab never sent still gets its machine: every three minutes the control plane asks GitLab for the waiting jobs of the projects that send theirs here, as it already asks GitHub, and it asks GitLab how a job went when its events do not come.
- A job started by the minute's sweep (one GitHub or GitLab never told of, one that waited for room) got a machine without the control plane's own links: no spot notice, no start report, no cache. It has them now.
- Two SuperCIs on one computer (the dashboard and a command) could renew the Cloudflare sign-in at the same moment, which ended it. One renews it at a time now, and the other takes what it renewed.
- A job no longer lands on a larger machine than it asked for when its first choice has no spot machine. In 0.17.0 the one request for the other pools also held larger sizes, and AWS, going by what it has spare, often gave one of those: a 2-CPU job on a 4-CPU machine, at twice the price. Larger sizes are now asked only after every pool of the size asked for said no.
- Not yet: a call of another workflow, a matrix made by an expression. `actions/checkout` puts the working tree in place as one commit; another repository is not checked out.

## 0.17.0 — 2026-10-14
- When the first choice has no spot machine, the others are asked in one request. Asked one by one, each pool (a machine type in a zone) that had none cost most of a second: a 64-CPU job with fifteen pools waited twelve to fifteen seconds for AWS to say no to all of them. One request for "any of these" answers in about three seconds whatever their number. AWS gives the machine from the pool with the most to spare, so the one least likely to be taken back. That is often an older machine type, and a job is over sooner on a newer one: the pools as fast as the first choice are asked first, and the slower ones in a second request only when none of those has a machine. The first choice is still asked by itself (about a second when it has a machine). A job's start says so: `no spot m8a.16xlarge; r8a.16xlarge in us-east-1d, AWS's choice of 14 pools`.
- This asks four more permissions of a control plane's AWS role: to ask for a spot machine from several pools at once, and to make, use and remove launch templates tagged as its own (AWS takes such a request's machine from a launch template; one is made for the job and removed again). A control plane in AWS gets them when it is updated. One in another cloud that starts AWS machines gets them on the dashboard's Permissions page or with `superci planes allow`. Until then pools are asked one by one, as before.
- GitHub and GitLab are answered before the machine is asked for (control planes in AWS). They wait ten seconds for an answer and count the event as failed after; with spot machines scarce, asking took longer, and such events showed as failed in `superci deliveries list` though the job got its machine. The job is claimed, the event answered, and AWS asked right after.
- A runner slow to reach GitHub says why. When a machine's runner takes more than ten seconds to listen for a job, the machine sends what the runner logged meanwhile: each wait of two seconds or more with the line before and after, and its warnings and errors. `superci jobs retrieve ID` shows it under the job's start, and `--json` has it as `start.runner_log`.
- A job cancelled as it is queued gets no machine. GitHub sends a job's events side by side, and the end of a job cancelled at once (a newer push took its place) can arrive before its start: the start then gave it a machine that sat idle until a sweep. The same for GitLab. A GitLab job cancelled while its machine is being asked for now gets none, or has the one AWS has just given ended at once with its runner removed (before, the machine idled for up to eight minutes).
- A job's record is written only over what was read of it. Requests run side by side (an event, a sweep, a machine), and one could hold a copy for seconds while it waited on AWS and write it back over what another had written meanwhile. A record that ended in between now stays ended, and a machine the older copy got since is ended. The tests cancel a job, start it and end it at every step of a start and of a sweep in turn.
- A Cloudflare sign-in that expired no longer stops everything. Signed in to AWS and Cloudflare, with Cloudflare no longer renewing its sign-in, every command that looks for control planes failed, also for a control plane in AWS. Now that sign-in is forgotten and said once (`superci planes list` adds a line, the pages show a notice with the button to sign in again), and what the other sign-ins reach goes on. What needs Cloudflare says to run `superci login cloudflare`. Updating a control plane whose runner agent is in Cloudflare updates the control plane and says the agent was left out. The same when Modal no longer takes its token.
- Update your control plane for these.

## 0.16.10 — 2026-10-14
- Every spot pool is asked before an on-demand machine. One request to the control plane asks twelve pools (a machine type in a zone) at most; a job with more to ask, three families in five zones, went on to an on-demand machine with three pools never asked. It now goes on with the rest a second later, and only then on down your order. A pool that had no machine within the last minute is not asked again, by that job or by another queued beside it: two jobs queued together no longer ask the same ten pools each.
- A zone AWS does not offer a machine type in is asked once, then not again for a day (each such request took most of a second, and came before the zone that had the machine).
- A machine for a job cancelled before any runner took it is ended at once. Its runner is taken back from GitHub first, which GitHub refuses while a runner is running a job, so a machine that was handed another job a moment before is left alone. Before, it waited for a sweep, up to two minutes on. A job cancelled while its machine is still being asked for gets none, or has the one AWS has just given ended in the same breath: a 64-CPU machine started that way had cost eight cents for nothing. Update your control plane for these.

## 0.16.9 — 2026-10-14
- Labels are written as key=value pairs. `runs-on: superci` is the default machine. A label that says a machine says it after its name, one `key=value` for each thing, in any order: `runs-on: superci/cpu=8/arch=arm64`. The keys: `cpu`, `ram`, `disk`, `disk-speed`, `disk-iops`, `arch`, `os`, `gpu`, `family`, `scratch`, `provider`, `spot`. Numbers have no units (`ram=32` is 32 GB). Each thing has one way to be said.
- The same keys everywhere. `superci machine update` takes each of them as a flag (`--cpu=8`, `--spot=false`, `--scratch=ramdisk`, `--family=m8a+r8a`; `--KEY=default` leaves one to each provider's standard again), and the label picker on Workflows writes them. The default machine's box reads "No spot machines". `--disk-speed` takes a number (125 is what AWS includes).
- `spot` is one key: `spot=false` for a job that must never be on a spot machine, and `spot=true` in a label undoes a default machine that says never.
- `scratch` is one key, for where what a job writes most is kept: `scratch=ramdisk` or `scratch=localdisk`. One or the other. A machine that has a disk of its own now uses it without being asked (a label naming such a family, `family=m8id`); `scratch=localdisk` asks for a machine that has one.
- `family` names the AWS machine families a job must have: `superci/cpu=32/family=m8a`, or several (`family=m8a+r8a`). Only those are asked for, spot and then on-demand as your order says: for a job that should always have the newest machines, whatever the day's prices. A family says its architecture (`family=m8g` is arm64).
- What does not go together is said at once, on the job's page: a key no label has (with the keys there are), a key said twice, a value out of its range, `arch=x64` with an arm64 family, `scratch=localdisk` with a family that has no local disk.
- A job's start is kept, step by step, so a slow one can be seen for what it was. `superci jobs retrieve ID` says how long each part took: GitHub telling SuperCI of the job, AWS giving a machine (every request and its answer: "no spot m8a.8xlarge in 6 zones; m7a.8xlarge in us-east-1a"), the machine being made, its boot, its disk and Docker, the runner's files read from the image, the runner reaching GitHub, and GitHub giving it the job. With `--json` a job has `start`, with each step in milliseconds. The machine says its own part to the control plane, with a link only it has. Before, a job kept when it was heard of, when its machine was asked for and when it began, and nothing between. Update your control plane for it.
- A spot machine is asked for where it costs least for the speed it gives. Every candidate type's price in every zone is read at once, and pools are asked from the least price up, a newer generation counting as a fifth faster than the one before (a job on it is over sooner). Before, the first type was asked in every zone, then the second: on a day when the newest type had no spot machine anywhere, every job spent seconds learning so, and then took the next in a fixed order, though another cost less. A zone AWS does not offer a type in is not asked for it. On-demand machines are asked for in the types' own order, as before.
- A job whose machine was not the first choice says so in its row: "Not the first choice: AWS had no spot m8a.8xlarge". Which machine type a job gets changes how long it takes and what it costs, and only the type was shown.
- A job says the disk it had. Its row and `superci jobs list` have the disk's speed beside its size when it is above what AWS includes ("32 CPU · m7a.8xlarge · 120 GB disk at 1000 MiB/s"), and "RAM disk" or "local disk" when its label asked for one. With `--json` a job has `disk`: its size, speed, IOPS, and whether its files were in memory or on a local disk. Before, only the size was shown, and the speed had to be measured inside the job.
- A job with `scratch=ramdisk` or `scratch=localdisk` keeps the disk speed AWS includes, at no extra price: its writes go to memory or to the local disk, and the faster disk was paid for and barely used. A speed said in the label or on the default machine is still given (`superci/cpu=32/scratch=ramdisk/disk-speed=500`). Update your control plane for it.

## 0.16.8 — 2026-10-13
- A machine's disk on AWS is as fast as its size deserves. Every disk had the speed AWS includes, 125 MiB a second, whatever the machine: a 32-CPU machine wrote no faster than a 2-CPU one, and a job that pulls Docker images or runs a database waited for its disk. The speed now follows the machine: 31.25 MiB a second for each CPU, up to 1,000. That is 250 on 8 CPUs, 500 on 16 and 1,000 from 32; the standard 4-CPU machine keeps what AWS includes, and costs what it did. Measured on real machines: sixteen copies of a 300 MB database made in 14 seconds on a 1,000 MiB disk where it took 110, a database's first start in 3 seconds where it took 26, and 6 GB of Docker images pulled in half the time at twice the speed. Update your control plane for it.
- What it costs is in each job's price: $0.007 an hour on 8 CPUs, $0.021 on 16, $0.055 from 32, a twentieth to a tenth of a spot machine's price. The default machine's Disk speed sets it for every job: Standard keeps what AWS includes at no extra price, a number gives every machine that speed (250 MiB a second on the standard machine is $0.007 an hour). In commands: `superci machine update --disk-speed=125`, `=250`, or `=default` to put it back.
- A label can say the disk's speed for one job: `superci/cpu=8/disk-speed=1000`, and `superci/disk-iops=6000` for IOPS (125 to 2000 MiB a second, 3000 to 80000 IOPS; SuperCI keeps them within what a disk of that size takes).
- `superci/scratch=ramdisk`: Docker's files, the job's work folder and /tmp in memory, for a job whose many small synced writes (databases, test set-up) no disk speed helps. They count as memory, up to three quarters of the machine's: ask for it on a machine with memory to spare. AWS, Linux.
- `superci/scratch=localdisk`: a machine with an NVMe disk of its own (AWS's `d` families: m8id, c8id, r8id, and m8gd on arm64), with the same folders on it. Ten times the small synced writes of a disk AWS attaches; for writing a lot at once a small machine's is no faster. Nothing on it outlives the machine. AWS, Linux.

## 0.16.7 — 2026-10-13
- A job on AWS has the disk it was given. The volume had the size asked for (60 GB, or what a label like `superci/cpu=8/disk=120` says), but the root filesystem stayed at the image's 29 GB, 18 of them used: a job had 11 GB free whatever it asked for, and a large one ran out of space. The machine now grows its root filesystem to the whole disk before the runner starts (under a second), on GitHub's and GitLab's jobs alike, and the C: drive on Windows. Update your control plane for it; a step that grows the disk by hand is no longer needed.
- A job run again after AWS took back its spot machine, in an account whose on-demand limit is too small for it, goes back on a spot machine. Before, it was kept on on-demand, where a machine larger than that limit can never start: it waited and then failed, while the spot limit it had just run under was free. The job says so: "On a spot machine again: an on-demand one does not fit your AWS account's on-demand limit".
- A job with no spot machine to be had now, whose on-demand machine the account's limit refuses, waits for a spot machine. Before, it was tried twice more and failed.
- A job waiting at the account's limit said "in use by 1 of your job there; it starts when one ends" when nothing of yours was running: the one counted was a machine that had ended a moment ago, which AWS counts a little longer. It now says that, and is tried again.
- A job on an on-demand machine because it was run again after a reclaim says "On-demand: run again after AWS took back its spot machine" (it read "AWS had no spot machine for it").
- A job's row has its disk beside its machine when it is not the standard 60 GB ("32 CPU · r8a.8xlarge · 120 GB disk"), in `superci jobs list` too.

## 0.16.6 — 2026-10-13
- Runs. The Jobs page has a Runs tab: a workflow's jobs together (on GitLab, a pipeline's), with how the run stands, how long it took from its first job queued to its last one over, what its machines cost together, and the job that was over last, the one the run waited for ("Last to end: E2E (1/2), 6m 12s after 1m 5s waiting for a machine"). A run or a job run again is its own row ("attempt 2"), for jobs from this version on. Only the jobs SuperCI ran are counted. In commands: `superci runs list` and `superci runs retrieve ID` (with `--json`).
- A control plane in AWS tries again within seconds. A job waiting for room, and a start AWS refused that is tried again, waited for the next minute's sweep; the control plane now asks itself for one twenty seconds on. Update your control plane for it (the update gives it leave to call itself, and nothing else).
- An AWS account that is its organization's management account is said so under AWS account, with signing in to another: it is easily picked by mistake where AWS lists the sessions.
- A project of AWS's new sign-up whose region does not have the runner image yet no longer reads "Everything SuperCI does works there" beside the step that comes first: it says the image is copied in first, and the Copy the image button says it is needed before Set up and takes about ten minutes.

## 0.16.5 — 2026-10-13
- A job on a spot machine AWS is taking back has what is left of AWS's two minutes to end by itself. Its runner is stopped twenty seconds before the machine goes (forty on Windows), and only if the job is still going. Before, the runner was stopped the moment the notice came: a job a minute from its end failed and was run again, where it now passes. A job with longer to go fails as before, and is run again once its run has finished.
- Several jobs queued at once, in an AWS account that AWS lets ask for few machines in a moment (a new one): four of five went to on-demand machines, at about twice the price, with nothing said of why. A spot request AWS refuses for its rate is now asked again for the same pool after a wait, in place of the whole launch starting over (which spent more requests and was refused again). One still refused after its waits goes on-demand as before, and the job says so: "On-demand: AWS refused the spot request for its rate (many machines asked for at once)".
- A spot pool (a machine type in a zone) that had no machine, or took one back, is asked last for a quarter of an hour. Each job of a burst no longer spends a request on it, and the job run again after an interruption is not followed by the next job into the same pool.
- The job list names each machine's row by the job it ran. GitHub gives a waiting job to any idle runner with its label, so jobs queued together run on each other's machines; a row was named by the job its machine was started for, and showed "E2E (2/2)" failed when it was "E2E (1/2)" on GitHub. The row's link and `superci jobs retrieve` go to that job too.
- The rows of the Limits card on Runners begin at the card's edge (they kept an empty place for a handle they do not have).
- A finished job is said by how the job itself ended: Passed, or Failed when its own steps failed (a test, a build), as GitHub said. Before, every job a machine had run to its end read "Done", in green, whether its tests passed or not. Jobs from before the update keep "Done": how they ended was not kept. In `superci jobs list` the same (`passed`, `failed`), and `--json` has it as `ended` beside `state`. Update your control plane for it.

## 0.16.4 — 2026-10-13
- The dashboard's tab has an icon: your GitHub organization's picture when the workspace has one, SuperCI's mark otherwise.
- The AWS account section reads as what it is. An account its organization keeps to some regions, with everything else allowed, says where SuperCI works ("SuperCI works there in N. Virginia") in place of "Restricted: some of what SuperCI does there is refused". The Runner image row says where the image is; a region the account may also use that lacks it is no warning, and has "Copy into Stockholm" beside it (before, the image could only be copied in from a terminal once a control plane was set up). A provider's region list says where to copy it in.
- An AWS sign-in that AWS ended early is seen as that. A sign-in lasts as long as the AWS session it was made from, twelve hours from when that session began; one made from a session eleven hours old has an hour. AWS then answered "expired" to ordinary calls before SuperCI went to renew, and the pages showed that as it came ("Not read: ExpiredToken…") and the AWS account section read "Nothing refused" with nothing known. Now the sign-in is renewed or, when it is over, the pages say so with Sign in again; and a look AWS did not answer is said as "Not known", never as an account with nothing refused. The page before AWS's sign-in says which session to choose.
- The GitHub App made for an organization is named after your CI and the organization, and nothing else: "AcmeCI acme" in place of "AcmeCI acme x1y2z3". It is what owners read among their Apps, and what a job run again by your CI says ran it. Should GitHub say the name is taken (an App of yours from before, never deleted there), another name typed on GitHub's page does as well. Apps made before keep their names.
- A GitHub App renamed on GitHub keeps working in the dashboard: the control plane asks GitHub for the App's address as it is now, and the links to the App (install it, its settings, its permissions) follow within ten minutes. Before, they went to the address the App was made with.

## 0.16.2 — 2026-10-13
- What an AWS account allows is asked again each time a page that shows it is opened (when the last look is older than twenty seconds), beside the page: a change made in AWS shows within seconds. Before, it showed up to ten minutes later. The AWS account section also has Refresh.

## 0.16.1 — 2026-10-13
- Removing SuperCI from an AWS account whose organization keeps it to some regions no longer reports "its machines' network" as left behind for the regions it may not use (nothing could be made there), with AWS's refusal in full.
- A control plane deleted from AWS no longer leaves its log behind: the function's last lines could make the log anew after it was deleted.

## 0.16.0 — 2026-10-13
- Daily limits, beside the monthly ones on Runners (the card is now called Limits): for everything, and for the AWS account. A day that runs away stops that day (UTC). In a terminal: `superci limits update --daily-usd=N` and `superci accounts update aws --daily-usd=N` (`none` lifts one).
- A budget in the AWS account itself, for the account's monthly limit: in the AWS account's limit dialog, "In AWS itself" is nothing, "AWS emails me at 80% and 100%", or "AWS emails me, and stops new machines at 100%". AWS counts the account's real bill, all of it and hours late, so it covers what SuperCI's estimate misses. In a terminal: `superci accounts update aws --aws-budget=emails|stops|none --email=ADDRESS`. It is made, changed and removed with your sign-in; the control plane gets no permission for it, and a budget of your own is never touched. Changing the limit changes the budget with it; removing AWS runners or the control plane removes it.
- With "stops", at 100% AWS puts a rule on the control plane's role that refuses new machines, and takes it off when the next month begins. A job refused by it fails on GitHub at once saying so ("AWS's budget for this account reached its limit and stopped new machines there"): the rule lets through one kind of machine, the small one that tells GitHub why a job was refused, and no job's own machine is ever that kind. Nothing is reported as a missing permission, and the account's row says "Stopped by AWS's budget" with Lift (`--aws-budget=lift`). Raising the limit lifts it too.
- Where your AWS account holds its own copy of the runner image (a region the image is not published in), SuperCI says when its maker has a newer build out: in the AWS account section, with "Copy the new one in", and in `superci images list`; `superci images update REGION` copies it in. Jobs start from the older copy until the new one is there, and the older one is then removed.
- A deploy that stopped partway and is given up ("Choose another place") removes what it had made in AWS. Before, its role stayed behind.
- A machine type a region does not have is no longer asked for with every job (about a second each time, and a spot machine's zone was then not chosen by price): the region is asked once what it has.
- A runner is named by your CI's name where developers read it (a job's log on GitHub, GitLab's runner list): `acme-ci-<id>-<job>` in place of `superci-<id>-<job>`. A CI given another name names its next runners by it.

## 0.15.0 — 2026-10-13
- AWS accounts that do not allow everything. SuperCI now looks at what an AWS account allows before it sets anything up there: which regions, whether spot and on-demand machines may start, how many CPUs at once, and whether the image jobs run on is in each region. It asks with reads and dry runs: nothing is made or changed by looking. Then it works with what it found. Nothing has to be changed in AWS to use SuperCI.
- What it found is an "AWS account" section on the Control plane, set-up and Add runners pages, for every account, and `superci accounts retrieve aws` in a terminal (`superci accounts list` lists the accounts signed in to). A refusal says whose rule it is: a policy of your AWS organization, named, or the permissions of the role signed in as. Where regions are chosen, one the account cannot use is shown and cannot be picked, with why. A deploy to a region the account cannot use is refused in plain words before anything is made.
- A project from AWS's new sign-up, which AWS keeps to one region and allows no spot machines until its advanced features are activated, is recognised and set up in its region.
- Where an account's AWS organization allows no spot machines, a job goes to an on-demand machine at once and says why: "On-demand: your AWS organization does not allow spot machines". Before, such a job failed. The refusal is remembered for ten minutes, so each job does not ask again, and AWS's spot provider on Runners says "not allowed" in place of "ready".
- The image jobs run on is not published in every region (not in Stockholm or Canada). Where it is missing, the set-up page offers to copy it into your account first, then sets up: about ten minutes, and about $1.50 a month for the copy's storage. In a terminal: `superci images list`, `superci images create REGION` and `superci images delete REGION --confirm`. Before, a control plane set up in such a region could start no job.
- Monthly limits, on Runners: one for everything (all providers together: at it no job starts until next month or a higher limit) and one for the AWS account (its spot and on-demand machines together: at it jobs go to the providers of other accounts). Each row shows what was spent this month. In a terminal: `superci limits update --monthly-usd=N` and `superci accounts update aws --monthly-usd=N` (`none` lifts one). A job refused at a limit fails on GitHub saying so. Update your control plane before setting one.
- Commands: a cloud or a code host is no longer a command of its own. `superci connections list`, `create github OWNER`, `create gitlab --url=…` and `delete` take the place of `superci github` and `superci gitlab`; `superci deliveries` takes the place of `superci github_deliveries`, and `superci projects` of `superci gitlab_projects`.
- With several workspaces and none named, the dashboard opens on the workspace it showed last (before: on the list of them); the switcher leads to the others. After switching workspace, the sidebar's links stay in it (before: the next press went back to the workspace the page was opened with).
- A job's machine is called by your CI's name, the repository and the job in AWS's console (`acme-ci-web-42`), in place of the control plane's id.
- A deploy that stopped offers "Choose another place" beside "Try again".
- Fixes: a finished set-up step no longer fills the screen; a page that refreshes itself leaves an open dialog, and a field being typed in, alone; just back from GitHub, the Connect button says it is connecting until the control plane has the App; a control plane that misses one answer is asked again before the page says "Not answering".

## 0.14.0 — 2026-10-12
- Workspaces: several CIs looked after from one computer, each apart from the others. A workspace has its own sign-ins to its clouds, and through them its own control plane, runners, repositories and settings. Nothing of one shows in another, and signing out of one leaves the others signed in. With one workspace nothing changes and nothing needs naming.
- The name at the top of the dashboard's sidebar is the switcher: your workspaces, each with its GitHub organization's picture once one is connected (GitLab's logo for GitLab), and "New workspace…", which asks what it is called (AcmeCI) and opens a clean first screen. The first control plane set up there gets that name. One dashboard serves them all; a page's address says which workspace it is of, so two tabs can show two. A sign-in comes back to the workspace that started it. The picture also stands beside the name at the top; it is asked for from GitHub when the dashboard starts and once a day, so a logo set there later shows by itself.
- A workspace has an id, made from its name (Acme CI: `acme-ci`): what commands take. Settings shows it, with Change, and, when there are other workspaces, "Remove this workspace": it is signed out and forgotten on this computer, and nothing in its clouds is deleted.
- Commands: `--workspace=ID`, or `SUPERCI_WORKSPACE=ID` for a whole terminal. With several workspaces, a command that names none stops, with nothing done, and lists them; an id that is none of them stops it too. Nothing picks one for you. `superci leave` takes the workspace from the command itself, never from the variable. `superci workspaces list`, `create NAME`, `update ID --id=NEW` and `delete ID --confirm`; `superci dashboard` with several and none named opens on the list of them.
- Sign-ins are kept by workspace, in `~/.superci/workspaces/ID/sign-ins.json`; the first workspace is `default`. A sign-in kept by an earlier version (`~/.superci/sign-ins.json`) is not read: sign in again (`superci login`), or move that file to `~/.superci/workspaces/default/`. Your control plane and runners are not affected.
- The switch between light and dark is gone: the dashboard follows your system's setting.
- Every dialog has one look, the one the Switch workflows and Connect GitHub dialogs had. A dialog opens with its field ready to type in, or with nothing marked.
- Inside one workspace, a sign-in to AWS adds an account beside the ones signed in to (the set-up page offers "Another AWS account"; in a terminal, `superci login aws --add`), for a control plane in one account and a place to move to in another; `superci planes create aws --region=… --account=ID` names the account. Each control plane is changed with the sign-in of its own account.
- A workspace is meant to have one control plane in use. If one has several that each serve a code host, its pages open on the list of them and carry the one shown in their address, and commands name theirs with `--plane=ID` (or `SUPERCI_PLANE=ID`); one that names none stops and lists them.

## 0.13.2 — 2026-10-12
- A job that AWS refuses because your account's limit of running CPUs is in use by your own jobs now waits for one of them to end, as it would on GitHub's own runners, and starts at the next sweep after. Before, it was tried three times in forty seconds and then failed. When nothing of yours runs there (the limit is below what the job asks, or something else in the account uses it), it still fails after three tries, and now says so plainly: how many CPUs it asked for, and the two limits to raise in Service Quotas, with their links.
- The runner that fails a job on GitHub with the reason is asked for as a spot machine and then as an on-demand one, in each of your regions (before: spot only, in the first region). So the reason is delivered even when the spot limit is the reason. Not when on-demand is turned off in your order.
- Jobs GitHub never told your control plane of are also looked for in repositories whose last job was up to two weeks ago, five of them at each look (before: only repositories with a job in the last day). The first push after a quiet weekend no longer rests on one event.
- Connecting an organization you do not own: the dialog offers a note to copy and send to an owner (GitHub lets only owners create an organization's App, and shows anyone else a 404). `superci github create OWNER --dry-run` prints the same note.
- Control planes of two teams reached from one computer (two cloud accounts signed in to, one after the other or side by side) can no longer damage each other. `superci leave` deletes the control plane it lets go of and the ones that serve nothing, and leaves one that serves a code host of its own (before: every control plane it found). A move refuses a target that serves a code host of its own (before: that one's GitHub App and settings were replaced). Delete refuses such a one as well. And a setting, an update or a delete is refused when SuperCI is signed in to another account than the control plane's own, saying which (before: it was written into the other account, or nowhere, and reported as done).
- `--plane=ID` is a check: a command stops, with nothing done, when the control plane it would act on is another one. `SUPERCI_PLANE` given without `SUPERCI_KEY` does the same, by id or by address. Before, the flag was accepted and ignored, and the variable alone too.
- `superci planes update ID` no longer makes the control plane it updated the one later commands act on.
- `superci leave --plane=ID --confirm` stops one control plane by itself: GitHub and GitLab stop sending it jobs and it is deleted, and the others go on as before. Whatever a `leave` or a delete leaves behind is named, with the command that removes it later.
- A setting that the control plane has not shown twenty seconds after it was written is said as that (before: reported as done).
- Each control plane is asked with a key of its own, made from the key kept on your computer and the control plane's address. Before, one key value sat in every control plane your sign-ins reached, so whoever could read one control plane's settings held a key the others took. A key from before goes on working until every control plane listed has been handed its own, then it is removed from them. `superci logout` now removes this computer's key from every control plane that has it (before: from one).
- A sign-in made by another SuperCI process on the same computer (a `superci login` in a terminal while the dashboard runs, a second dashboard) is taken in by a running dashboard at its next page. Before, the dashboard's next write put its own older state back and undid that sign-in. `superci logout` elsewhere signs a running dashboard out too.
- Cloudflare runners for a control plane that lives elsewhere go into the one Cloudflare account your sign-in reaches, or the one named (`superci runners create cloudflare --account=ID`); with several and none named, SuperCI asks which (before: the first one). The Control plane page says a control plane is signed in to only when the sign-in is for its own account.
- A second dashboard on the same computer works. With port 8976 taken, `superci dashboard` and `superci login aws` use the next free port and say so (before: "port 8976 is in use"). Signing in with AWS, Modal or GitHub comes back to that port (before, AWS's always came back to 8976, into the other dashboard). Each dashboard keeps its own tab signed in (before, opening one signed the other's tab out). Only Cloudflare's browser sign-in still needs port 8976 itself, and says so.

## 0.13.1 — 2026-10-12
- A Settings page, below Control plane in the sidebar: the name, the labels and the limits (largest machine, longest job, public repositories) moved there from Workflows, and so did signing out, which was under More. Workflows keeps the default machine and how to use your label.

## 0.13.0 — 2026-10-12
- Many jobs queued at once no longer leave a machine idling and a job called failed that passed. GitHub gives a waiting job to any idle runner with its label, so jobs often run on each other's machines; with three or more passing theirs around, the control plane could start one machine too many, let it idle for ten minutes, and then say "Its machine never started the job" of a job that had run elsewhere (seen with 16 jobs at once). No such machine is started now; a machine that ended with another job's work keeps its cost in the list; and a machine nobody waits for is ended at the first sweep after it had time to come up (a minute and a half on AWS), not after eight to ten minutes. Before ending one, the control plane asks GitHub whether its runner is busy, and a job that still waits with no machine coming gets that one.
- A burst of launches that AWS refuses for their rate ("RequestLimitExceeded", seen from the eighth job of a burst on) is asked again within the same seconds. Before, those jobs waited for the next sweep, up to two minutes.
- A control plane in AWS sweeps every minute (before: every two). `superci planes update` changes the schedule.
- A job that ran on an on-demand machine because spot was refused says so, in its row and in `superci jobs list`: "On-demand: your AWS spot limit in N. Virginia was reached". When that happened in the last day, a line above the jobs says how many, with the link to ask AWS for more. Before, only "on-demand" beside the machine showed it, at about twice the price.
- `superci login aws` opens on a page of SuperCI's with one button to AWS. When AWS answers “400 Bad Request” (an old AWS sign-in in the browser is in the way), Back comes to that page again, which then offers to sign out of AWS and try again.

## 0.12.0 — 2026-10-09
- Labels of your own. Your control plane answers to a list of labels, `superci` alone to begin with: add one on Workflows → Labels, or with `superci labels create acmeci`, and workflows can name it in `runs-on` (`runs-on: acmeci`, `acmeci/cpu=8/arch=arm64`). Every label in the list works at once, so workflows change one at a time and nothing waits the day you add one; remove a label when nothing names it any more. The one added last is the one shown in examples. Two to twenty-four lowercase letters and digits.
- A name of your own. Workflows → Name, or `superci name update AcmeCI`: the dashboard's header and tab say it, a GitHub App made for a further organization starts with it (AcmeCI acme …), GitLab's runners are described with it, and a job that could not be run says "AcmeCI could not run this job". `SuperCI` puts it back. The command stays `superci`.
- A move to another control plane takes the labels and the name along.
- Installs without Node too, as the same program: `brew install supercorp-ai/tap/superci`, `curl -fsSL https://superci.dev/install.sh | sh` (macOS and Linux; checked against npm's checksum, put in `~/.local/bin`), or `cargo install superci`. Then `superci dashboard`.

## 0.11.2 — 2026-10-09
- A job that GitHub never told your control plane of now gets its machine all the same. GitHub sends each event once, and can lose one (seen: a job queued four seconds after a move changed the App's address was never sent anywhere, so it waited until cancelled). The control plane now asks GitHub which jobs wait for its runners: every five minutes for the repositories that had a job in the last day, and every sweep in the quarter of an hour after a move.
- After `superci planes move`, commands read the control plane moved to at once (before: the one moved away from, until `superci planes list` or the dashboard had looked again).
- `superci github_deliveries list` and `retrieve ID` show what GitHub says of the events it sent your control plane: when, what, how each was answered, and for one, which job, which address and what was answered. For a job that never got a machine.

## 0.11.1 — 2026-10-08
- `superci planes create`, with a control plane in use already: the new one no longer takes its place in what SuperCI keeps on your computer. In 0.11.0, commands run afterwards went to the new, empty control plane until the dashboard was opened again.
- After AWS has ended a sign-in that was SuperCI's only one, the dashboard and the commands that look go on working at the next start too (in 0.11.0 only until the program ended), and a change says that the sign-in has ended and to run `superci login aws`.
- A move to another control plane waits until the new one says every runner provider it was given is there, before GitHub and GitLab are switched over. Before, a job that arrived in the first minute could fail saying a provider was not added (seen with Cloudflare's containers).
- A command that would delete something and was not given `--confirm` says only that, without its resource's help after it.
- `superci login` says what it is waiting for and that it is done, without a line for each request of the browser.

## 0.11.0 — 2026-10-08
- SuperCI stays signed in. Its sign-ins to your clouds are kept in its own folder on your computer (`~/.superci`, readable by you alone), so the dashboard opens where you left it. An AWS sign-in still ends after twelve hours at most, as AWS has it; Cloudflare's no longer ends after an hour.
- Everything the dashboard does is also a command, for scripts and coding agents, in the shape of Stripe's CLI: `superci <resource> <operation> [id] [--param=value]`, with `list`, `retrieve`, `create`, `update` and `delete` wherever they fit. The resources: `jobs`, `planes` (also `move` and `allow`), `runners` (also `order`), `machine`, `limits`, `public_repos`, `github`, `gitlab`, `gitlab_projects` and `keys`; beside them `superci status`, `login`, `logout`, `dashboard` and `leave`. A command runs the same code as its page. `--json` prints one object; `--dry-run` checks a change and says what it would do; nothing is ever asked in the terminal; what would delete something needs `--confirm`; when a person is needed first (a sign-in), the answer names the command for it and the status is 3.
- The dashboard opens with `superci dashboard` (`npx @superci/cli dashboard`). `superci` alone lists the commands, and `superci help RESOURCE` (or `superci RESOURCE --help`) says more about one.
- `superci login [aws|cloudflare|modal]` signs in, in the browser, and ends; `superci logout` removes the sign-ins from this computer, asks Cloudflare to end its one, and has the control plane forget this computer's key. The dashboard's sidebar has the same under More → Sign out.
- After an AWS sign-in has ended, the dashboard still opens on your control plane and shows its jobs and settings (read with this computer's key), with a line saying the sign-in ended and a button to sign in again. Before, it would have shown the first screen as if nothing were known.
- SuperCI reads no other tool's credentials. `CLOUDFLARE_API_TOKEN`, `MODAL_TOKEN_ID` and `MODAL_TOKEN_SECRET` are no longer picked up; on a machine with no browser a sign-in is given by name: `SUPERCI_CLOUDFLARE_TOKEN`, `SUPERCI_MODAL_TOKEN_ID`, `SUPERCI_MODAL_TOKEN_SECRET`.
- The key the dashboard reads a control plane with lasts thirty days and is reused between runs (before: a new one, twelve hours, at every start).
- Keys that only read, for a coding agent or a script that should look and not change: `superci keys create agent` shows one once; with `SUPERCI_PLANE` and `SUPERCI_KEY` set, whatever looks (`superci status`, `superci jobs list`, `superci jobs retrieve ID`…) works with no sign-in on that machine, and nothing can be changed. Your control plane keeps only the key's SHA-256; a key ends by itself (30 days unless said otherwise) or with `superci keys delete`.
- `superci jobs retrieve ID` shows one job with the end of its log: GitHub's, read by your control plane with your App's token, or GitLab's. So why a job failed can be read without opening the code host.
- A change is seen at once on a control plane in AWS (before: up to fifteen seconds later, while it went on with the settings it had read). The dashboard and commands no longer wait there after each change, and a change made right after another starts from it.
- A job may run for six hours, as on GitHub's own runners (before: 70 minutes). Limits has a new setting, Longest job (`superci limits update --max-hours=N`), for another length: up to five days on AWS (the longest GitHub lets a job run on a runner of your own), a day on Modal; Cloudflare keeps a container six hours at most. A job's machine is ended at that limit, also when the control plane is gone.
- A release becomes what `npx @superci/cli` gives only once every one of its files downloads from npm, the launcher's too (npm lists a version minutes before it serves its file; for those minutes a new install failed).
- The dashboard has a new mark: four petals, as on superci.dev, in place of the two arrows.

## 0.10.7 — 2026-10-06
- `npx @superci/cli` works on Windows (x64, and arm64 through Windows' own x64 support). A release tries the package on both before it is published.
- Every push and pull request to the repository is built and tested on GitHub.

## 0.10.6 — 2026-10-06
- SuperCI is released by its repository on GitHub: one workflow builds the program for macOS and Linux (Apple silicon, Intel, arm64, x64), runs the tests, tries it through `npx`, and publishes `@superci/cli` to npm, which trusts that workflow (trusted publishing). No person's npm login or token is part of a release.
- Nothing changes in the program or in a control plane.

## 0.10.5 — 2026-10-06
- Docker works in jobs on AWS: service containers, container jobs and `docker` steps. The machine image left Docker to root, so these failed; the runner's user is now let in before the runner starts, and Docker starts beside it.
- A GPU job in an AWS account that is allowed no GPU machines fails at once saying which allowance to raise, also when spot machines were refused for another reason (before: after three tries, with "AWS had no room").
- Both found in the first runs on real clouds.

## 0.10.4 — 2026-10-06
- Back from creating and installing the GitHub App, the dashboard reads your control plane fresh and keeps looking until it says the App is installed (a control plane takes a few seconds to see what was just stored). Before, Overview could go on saying "Connect repositories" after GitHub was connected.
- A page shown from what the dashboard read a moment ago looks again once that has been read anew, so it does not stay on what was true before.

## 0.10.3 — 2026-10-06
- Set up on Overview does nothing in place: each step says where it stands in a line and has one button to the page where it is done (Set up control plane, Connect repositories, Add runners, Open Workflows).
- Deploying a control plane has its own page, Setting up your control plane, with its steps; when it is done you are back on Overview. Overview says a deploy is running in one line, with a link there.
- The deploy's steps say what each one is (its permissions, its storage, its check for waiting jobs and leftover machines every 2 minutes on AWS).
- Runners: the AWS on-demand row says "one per job, never interrupted". Like every machine, an on-demand one is ended when its job ends.

## 0.10.2 — 2026-10-05
- Before there is a control plane, Overview is the page it will be: Set up with its first step open (one button, Set up control plane) and the steps after it in one row, then the day's figures and the latest jobs, still empty.
- An Overview with no jobs yet shows the same shapes at zero instead of one line.
- Dark mode is darker: near-black with neutral greys and white text, as on supercov.com.

## 0.10.1 — 2026-10-05
- When AWS has no spot machine for a job, the order of your runner providers decides what happens: the job goes to the next provider in the list that can run it. AWS on-demand is a row of its own there (Runners), right under AWS until you move it, with its own limits (jobs at once, a monthly budget) and a switch to turn it off.
- The same when a provider cannot start a job's machine for any other reason (Cloudflare or Modal failing, AWS refusing the request, a provider refusing what the job asks, like GPUs): the job goes to the next provider that can run it instead of trying the same one again. When none can, each one's answer is said; the job is tried again twice, starting with the providers that have not failed it, then fails.
- With nothing below that can run the job (on-demand off, no other provider that fits), it waits for a spot machine instead of failing.
- A job run again after AWS took its spot machine back, and jobs started while spot is paused after several were taken, also go to the next provider below AWS's spot machines (before: always on-demand).

## 0.10.0 — 2026-10-04
- The name is SuperCI. The program is `superci`, the label is `runs-on: superci` (with sizes: `superci/cpu=8`, `superci/gpu=any`, …), and what it makes in your clouds is named after it: control planes `superci-plane-<id>`, runner agents `superci-runners-<id>`, the GitHub App `SuperCI <owner>`, the tag `superci-plane`.

## 0.9.37 — 2026-10-04
- Several GitLabs on one control plane: Repositories → Another GitLab adds another server, or another account's token on the same one, beside the first. Each is a row with its own settings (its projects, a new token, Disconnect) and its own line under Permissions; their jobs run on the same runners. Jobs and runners of different GitLabs are kept apart even when their numbers are the same.
- A GitLab connection's token can be replaced in its settings (New token) without disconnecting: the projects turned on stay on.
- A move takes every GitLab connection along.

## 0.9.36 — 2026-10-04
- Control plane → Permissions reads at a glance: each row says in a few words how it stands, and when there is something to do, one Review button opens its steps (what is needed and why, then the one button or link that does it).
- "Change region" beside AWS is a button, like the others in its row.
- Connecting GitHub (the first organization or another) and GitLab are dialogs of three steps: where it is (the public one, or your own, which asks its address), whose jobs or which token, then the button that does it. Their rows have one button each.
- AWS's sign-in page answering "400 Bad Request" (it does when the browser holds an AWS console sign-in that has ended; AWS's own `aws login` has the same fault): back in the dashboard, a notice says the two ways on: sign in to the AWS console in another tab and try again, or "Sign out of AWS and try again", which clears the old sign-in and goes on to the sign-in.
- Signing in with AWS from a Review dialog comes back to that dialog, with everything to allow.
- Signed in to another AWS account than the one a control plane's role is in: Permissions says which is which.
- An AWS sign-in that has run out (they last twelve hours at most) is forgotten and asked for again, not shown as an error on every action.
- After an update, while its cloud still answers with the version before (Cloudflare can take a few minutes with containers), the dashboard says "Restarting" and looks again by itself, with no Update button meanwhile.

## 0.9.35 — 2026-10-04
- Fixes found by a second review, of the three versions before:
  - A job whose runner was taken by another job gets a machine even if that other job's machine never comes up (before, it could stay queued).
  - Out of time while spot is refused: the job's next try goes straight to on-demand.
  - A job's end delivered twice by GitHub no longer ends the machine kept for the job that waits; a machine left idle by a cancelled job that takes another job is not ended under it.
  - A network of your own set or changed in the dashboard is used by the next job, not minutes later.
  - Two organizations of the same name on github.com and on a GitHub Enterprise Server are kept apart.
- Several jobs of one run taken back by AWS are run again together with that run's other failed jobs (GitHub takes one request for a run's new attempt).

## 0.9.34 — 2026-10-04
- GitHub Enterprise: an organization on your own GitHub Enterprise Server (3.10 or newer), or on GitHub Enterprise Cloud with data residency (yours.ghe.com), connects like one on github.com. When making its App, say where your GitHub is (Repositories → the field beside the name; empty: github.com). Its App, its jobs' runners and every link then use that server. One control plane can serve organizations on github.com and on your server at once.
- What it needs: the server can reach the control plane (it sends job events there), the control plane can reach the server's API, and runners can reach the server (on AWS, a network of your own does that for a server inside your network).
- A Cloudflare control plane hands its settings to its Durable Object in pieces, so many organizations' Apps fit.

## 0.9.33 — 2026-10-04
- GitHub's full image on Cloudflare (experimental) is checked: every piece against its SHA-256 in the image's index; the index against the address, when the address ends in the index's checksum; the reader program against the index. A piece that is not what the index says is never used.
- An image that cannot be loaded fails the job at once saying why, and what was set up is taken down (before, the job ran in the small image, without the tools it expected).
- The reader survives what goes wrong: a piece that cannot be fetched fails only the reads of it, and is tried again when read again; its cache gives way when the job fills the disk (the oldest pieces are dropped, and fetched again if read again).
- `image-reader pack <image file> <directory>` makes what to upload: the pieces, their checksums, the hot list, the reader, in a directory named by the index's checksum. `crates/image-reader/try.sh` tries all of it for real in a local Linux container.
- The image's environment (/etc/environment) is read as names and values, never run.

## 0.9.32 — 2026-10-04
- No room for a spot machine in any region: the job gets an on-demand one (the regions in order again) rather than waiting; its machine says "on-demand" in the job list.
- Spot machines taken back: Windows machines and GitLab jobs are handled too. A Windows machine ends the job's process so it fails at once; a GitLab job is run again by GitLab at once, on an on-demand machine. Several jobs of one GitHub run taken back are run again with one request.
- Your own network on AWS: Runners → AWS → Your own network takes a line a region (subnets, security groups, `private` for subnets without public addresses). Machines there start in your network, so jobs reach what it reaches and leave by its NAT. A network that is wrong is said, and no machine starts anywhere else. No new permission.
- Fixes found by a review of the last versions:
  - A job that passed just as AWS gave notice is not run again; a notice follows the machine to the job its runner took; re-runs survive GitHub not answering.
  - A failing runner that takes another job is said on that job, and that job's machine takes the first.
  - GitLab: a runner that takes another job's job no longer has its busy machine taken for one that never began; a job cancelled before a runner took it has its machine ended.
  - A machine that never began a job GitHub or GitLab no longer has waiting is ended, with none started again; a machine's age counts from its start, not from when its job arrived.
  - A network that cannot be read is an error (the job is tried again), never a reason to use AWS's default network.
  - Removing AWS deletes what lets the control plane start machines first, then its machines and network; a sign-in that ended is said before anything changes.
  - Adding an organization never replaces the first one's App on a guess; removing one forgets it even if GitHub will not uninstall its App; a move takes every App and waits for each.
  - `os=windows` and GPU labels no longer take a default machine's arm64 with them.

## 0.9.31 — 2026-10-04
- Several GitHub organizations on one control plane: Repositories → Another organization makes a private App in that organization too (a private App belongs to one account), kept beside the first; its jobs run on the same runners, with the same limits. Each organization is a row (Install, Manage, Remove), and Control plane → Permissions says for each what its App asks for and what was accepted. Removing one uninstalls its App there; the App itself is deleted on GitHub.
- A move takes every organization's App along.

## 0.9.30 — 2026-10-04
- Cloudflare (experimental): jobs can run inside GitHub's full runner image, which a container's 20 GB disk cannot hold. Runners → Cloudflare → GitHub's full image takes the address the image is published at; the container shows it as one file and fetches only what a job reads (several pieces at once, the commonly used ones first), with a writable layer on its own disk. GitHub's runner starts inside it as on GitHub's machines (same tools, tool cache and environment). Empty: the small image, as before; if the image cannot be reached, the small image is used.
- New program `image-reader` (Linux): shows an image kept as small pieces at a public address as one local file.

## 0.9.29 — 2026-10-04
- AWS taking back a spot machine mid-job: the machine tells the control plane and stops its runner, so the job fails at once (GitHub otherwise waits many minutes for a runner it no longer hears from). Once the run has finished, that job is run again as a new attempt (with the jobs that need it), on an on-demand machine. GitHub's App needs Actions (write) for that: Control plane → Permissions says how; without it the job says it was not run again, and why.
- Two spot machines taken back within a quarter of an hour: jobs start on on-demand machines for half an hour.

## 0.9.28 — 2026-10-03
- AWS spot machines try each zone of a region, cheapest first, also in the region's default network (before, AWS picked one zone, and a full zone meant the next region).
- A job AWS had no room for says what each region answered; "no GPU quota" only when every region refused for quota.

## 0.9.27 — 2026-10-03
- Workflows → Switch workflows: a prompt to paste into your coding agent (Claude, Codex, Copilot, Gemini) that moves a repository's jobs to SuperCI: every workflow, or one workflow first. Short prompts: what to change, what stays on GitHub (with why), one pull request (or GitLab merge request) with a table of the changes.

## 0.9.26 — 2026-10-03
- Signing in with Modal: Modal's page does not come back to the dashboard by itself, so while it waits every page looks again and notices the approval; Control plane → Permissions says to approve in Modal's tab and close it.

## 0.9.25 — 2026-10-03
- Runners → a provider's settings → Remove: jobs stop going there at once, then what SuperCI made for it in that cloud is deleted with your sign-in there (AWS: its machines, their network and the control plane's role; Modal or Cloudflare: the runner agent). The control plane and your sign-ins stay; adding it back is Add provider. Not signed in there: sign in from the dialog, or remove it from SuperCI only (what stays is listed). Jobs running there are stopped, after saying so.
- A control plane in AWS can remove its AWS runners too (its own role keeps its control-plane policy) and add them back.
- Control plane → Permissions: Modal's runners work without a sign-in; it says so.

## 0.9.24 — 2026-10-03
- The runner that fails a job on GitHub (see 0.9.15) is tried on each place in turn, and again each minute for half an hour while GitHub still has the job queued; before, a container that would not start left the job queued for a day.
- Overview: jobs from computers (from before they were set aside) are named so.

## 0.9.23 — 2026-10-03
- A job whose machine would not start, three times, now fails on GitHub saying why, instead of staying queued there for a day.
- A cloud refusing what a job asks (Modal: GPUs need a payment method on the workspace) fails it at once with Modal's own words.
- AWS GPU jobs say which quota to raise even when a later region has no GPU image.

## 0.9.22 — 2026-10-03
- GPUs: `runs-on: superci/gpu=any` gets one NVIDIA GPU (the least costly: T4), or name one: `superci/gpu=l4`, `gpu=a10g`, `gpu=l40s` on AWS or Modal, `gpu=a100`, `gpu=h100`, `gpu=h200`, `gpu=b200` on Modal. AWS uses RunsOn's image with NVIDIA's drivers; Modal's sandboxes get the GPU, priced per second.
- Windows: `runs-on: superci/os=windows` runs on AWS (Windows Server 2025, x64), spot by default, priced at Windows' rates.
- An AWS account that allows no GPU machines (AWS's quota for them starts at 0) fails GPU jobs at once, saying which quota to raise, instead of trying for a day.
- The machine picker (Workflows) offers Windows and GPUs.

## 0.9.21 — 2026-10-03
- AWS machines start in a network of their own per control plane and region, with nothing let in: jobs can't reach each other or your other machines in AWS. The dashboard makes it when AWS is connected, when a region is added, with an update, or with Control plane → Permissions → Review; it is removed with the control plane. Until then, machines use the region's default network as before.
- AWS spot machines start in the region's cheapest zone for their type, then the next when it has no room.
- GitLab: each job gets a runner of its own, paused once it takes its job and removed after (runners were shared by jobs with the same tags). Runners from before are removed.

## 0.9.20 — 2026-10-03
- Your own computers are coming soon: until jobs on them can be kept apart from the computer as reliably as on a cloud, they can't be added. The computer agent (`superci join`) and its routes are gone; a macOS job fails at once, pointing to GitHub's macos-latest.

## 0.9.19 — 2026-10-02
- A macOS VM's runner works in a folder beside itself (macOS has no /home/runner).

## 0.9.18 — 2026-10-02
- Your Mac runs macOS jobs in a VM of its own per job (Tart, on Apple's virtualization; Cirrus Labs' plain macOS image), deleted after the job, so nothing a job does stays on the Mac. Two run at once at most. Jobs wait while the Mac fetches the image (once, about 27 GB).
- Running jobs on a computer itself (as you, with your files) is off unless turned on for that computer.
- A computer's jobs run in a folder without spaces in its path (GitHub's runner does not quote a step's script).

## 0.9.17 — 2026-10-02
- `superci/os=macos` means Apple silicon (arm64), as GitHub's macos-latest does; `superci/arch=x64/os=macos` asks for an Intel Mac.
- A macOS job with no Mac added fails saying so, and how to add one (or to use GitHub's macos-latest).

## 0.9.16 — 2026-10-02
- Workflows → Limits lists the public repositories GitHub's App is installed on (jobs come only from those), each allowed with one click, instead of typing names.

## 0.9.15 — 2026-10-02
- A job nothing here can run (a machine no place has, like macOS with no Mac added; one larger than allowed; a place not added) fails at once on GitHub, saying why, instead of waiting a day: a small runner takes it and fails it before any step. A GitLab job like it is cancelled.
- A job waiting for a computer that went offline (the only place that can run it) fails after five minutes, the same way.

## 0.9.14 — 2026-10-02
- Control plane → Permissions: for AWS, GitHub and GitLab, whether they allow what this version asks for, or what is missing, why, and how to give it. AWS's role is read while signed in to AWS and given what it lacks with Review → Allow; not signed in, what AWS refused lately is shown. GitHub: the App's and each installation's permissions, with links to give them. GitLab: the token's scopes.
- An update lists the permissions it also asks for (sidebar and Changelog), and gives AWS's what it lacks when signed in to AWS.
- AWS's permissions are made from one list in the code, each with what it allows, why and since when; the CloudFormation template (no longer used) is gone.

## 0.9.13 — 2026-10-02
- Each provider on Runners shows this month as its cloud bills it: Cloudflare's containers after the usage Workers Paid includes, Modal's apps after the plan's credits (from Modal's own billing), AWS at the prices it billed.
- An AWS machine's cost includes what it sent (CloudWatch's count, at AWS's price for data sent out), added a few minutes after it ends.
- A Modal control plane no longer keeps a container up around the clock: its sweep's container goes as soon as it is done (it cost about $5 a month idle).

## 0.9.12 — 2026-10-02
- Costs are settled when a job ends. AWS: at the prices AWS billed for its time — its zone's spot price for each hour (or the on-demand list price), its disk at the region's gp3 price, and its public address ($0.005/h) — a minute at least. Cloudflare: as Cloudflare metered its container, read by the dashboard while signed in to Cloudflare. Until then a cost is marked as estimated (≈).
- Modal sandboxes are sized in CPUs as GitHub counts them (two to one of Modal's cores) and held to what they ask for, so each is billed exactly its size.
- The comparison with GitHub uses GitHub's arm64 prices for arm64 jobs; job costs show to the hundredth of a cent.

## 0.9.11 — 2026-10-02
- Jobs from public repositories are refused unless the repository is allowed (Workflows → Limits); runs from a fork's pull request and `pull_request_target` runs are refused even then.
- A label asking for a machine larger than allowed (32 CPUs unless set otherwise in Workflows → Limits) is refused; every cloud runs at most 20 jobs at once unless set otherwise.
- Monthly budgets count on-demand AWS machines (estimated from their size) and the time a runner waits for its job; a Cloudflare container's wait counts its memory and disk (CPU is billed as used).
- An AWS machine powers itself off at its time bound twice over (a timer and a scheduled shutdown), so a job ending the timer no longer keeps it up.
- A control plane that moved forwards only events signed by GitHub or GitLab; job history older than two months is removed.
- The dashboard answers only on this computer's own address and makes changes only from its own pages.

## 0.9.10 — 2026-10-02
- Moving to an AWS or Modal control plane switches over without waiting for it to read its settings again (it reads them fresh for a move's steps); a move brings Cloudflare's Location along.

## 0.9.9 — 2026-10-02
- Changes saved for a Cloudflare control plane take effect in seconds (its Worker hands the Durable Object the settings as they are now; the Durable Object kept the ones it started with for up to half a minute).

## 0.9.8 — 2026-10-02
- A runner that takes a job other than its own (GitHub gives a waiting job to any runner with the label) no longer leaves its own job waiting forever when the other job's machine had failed: the job it was started for gets a new machine.

## 0.9.7 — 2026-10-02
- Cloudflare has a Location in its settings: where its containers start (eastern North America by default, near GitHub and GitLab; another area; or Automatic, where Cloudflare chooses).
- Region and location lists mark the one closest to the code hosts you connected (GitHub, gitlab.com).

## 0.9.6 — 2026-10-01
- A Cloudflare control plane takes a new dashboard session's key at once (its Worker reads the secrets fresh and vouches for the key; the Durable Object kept the secrets it started with for about 15 seconds).
- After a move or an update the page reads afresh, so the one in use shows at once; a move says where it went.
- AWS's regions are a list in its settings: drag to reorder, add one, remove one.
- Saving a dialog shows Saving… on its button until it is done; a control plane that does not answer for a moment (a Worker restarting after a setting changed) keeps its last view, so its runners do not seem to vanish.
- Reordering runner providers saves in place ("Saving the order…", then "Order saved"); the page does not reload.

## 0.9.5 — 2026-10-01
- A faster first look: the dashboard searches every cloud at once and hands each control plane its key at once; an AWS or Modal control plane takes a new dashboard key at once (it read its settings every 15 or 10 seconds before).
- An AWS control plane runs with 1024 MB (it had 256 MB, about a seventh of a CPU): it starts and answers several times faster. Update sets it on one set up before.
- No more flicker while a page waits: it is redrawn only when something changed.
- Overview has no control plane chip; Workflows' default machine is one card, and Choose a machine… picks another.

## 0.9.4 — 2026-10-01
- A job whose label names a runner provider this control plane has not added says so plainly ("its label needs Cloudflare runners, which this control plane has not added").
- Workflows: the default machine first; then the steps for GitHub Actions or GitLab CI; another size from Make a label…, a dialog.
- States read as words with a capital ("Connected", "Ready"); the ⋯ menu no longer nudges its row.

## 0.9.3 — 2026-10-01
- Moving is non-destructive and runs in the background, its steps in its row. Your runner providers come along (Cloudflare's containers, Modal's sandboxes, AWS), and so does job history; nothing is removed from the old control plane, so you can move back. Move here first says what comes along and what needs a sign-in.
- The switch is safe: the new control plane waits on standby until GitHub and GitLab are pointed at it, and the old one passes on any event that still reaches it.
- Delete a control plane that is not in use (its Worker, function or app and storage, and the runner agents made for it); Stop using SuperCI lets GitHub and GitLab go and deletes everything from your clouds.
- Control plane is one list: the one in use, others to move to, and each cloud without one with Set up (its progress in its row). Delete and Stop using SuperCI are in each one's menu (⋯).
- Overview: no "Needs a look" list (each job's row says what went wrong); GitHub's price behind a small mark; a quiet line when there are no jobs yet; pages no longer flash "Reading your control plane" when one is slow to answer.

## 0.9.2 — 2026-10-01
- One control plane is in use at a time. Another (in another cloud or account) is somewhere to move to: Move here brings your GitHub App and its installations, GitLab and its projects' webhooks, the order of your runner providers, the default machine and your computers; jobs running at the time finish where they are.
- Every control plane answers to the same label (`runs-on: superci`), so workflows do not change on a move. A cloud account holds one control plane.
- A job whose machine's start was cut off midway (the control plane stopped while a cloud answered) is placed again after two minutes, twice at most; before, it stayed queued until GitHub redelivered its event.
- Your computers follow a move by themselves. Runner providers tied to a control plane (AWS, Cloudflare, Modal) are added again on the new one; Runners says which.

## 0.9.1 — 2026-10-01
- Jobs keep their names and their workflow's (GitLab: their stage), so lists say "test · app · ci" rather than numbers.
- Overview, rethought: jobs in a table (name, runner in words, time, cost, how it ended); what needs a look grouped by what happened, in plain words; where jobs ran only when more than one provider ran them. The control plane is one quiet chip with its own icon.
- Cloudflare's errors are recorded by their message.

## 0.9.0 — 2026-10-01
- AWS machines can go to several regions in order: when one has no spot capacity (or quota) left, the next. Adding AWS sets US East (N. Virginia), closest to GitHub with the most spot capacity, then Ohio and Oregon; change them in AWS's settings on Runners.
- Adding AWS says what your account may run there (its spot CPU quota, as jobs of 4 CPU) and links to AWS's page to ask for more when it is low. Regions show their full names.
- Workflows, in the sidebar: how jobs ask for your runners, for the code hosts you connected, with the default machine and the picker for another.
- The control plane and What's new are pages of their own (Settings before); Overview says which line is your control plane.

## 0.8.1 — 2026-10-01
- A machine that fails to start (Cloudflare's "temporarily unavailable", a spot shortage) is tried again, twice, instead of failing the job; a start always gets a fresh container.
- Overview, after supercov's report: the control plane and your label on one quiet line; Jobs (how many got a machine, the median wait for one, what they cost against GitHub's runners, what needs a look, the latest); where jobs ran.
- Every page waits in the shape of its own cards, the loader inside the first.
- Overview's measures: jobs in the last day with a chart by hour (those that ran, those that did not), the median wait for a machine, and spend next to GitHub's price for the same jobs. Pages' headers no longer float.
- Repositories: a Use it card switching between GitHub Actions and GitLab CI, with the steps for each; GitLab's projects and connection are its own settings page (Settings on its row).
- GitLab projects say when GitLab paused or disabled their webhook, and when the last event from GitLab arrived; Turn on all.

## 0.8.0 — 2026-10-01
- GitHub jobs with `services:` and `container:` (and `docker://` steps) run on Cloudflare: Docker starts when a job first uses it, and containers share the machine's network (Cloudflare has no bridge networks). Service names and `job.services.<name>.ports` work as on GitHub; two services on one port would collide.
- Runner pools are called runner providers (Add a provider).

## 0.7.0 — 2026-10-01
- GitLab CI, next to GitHub: connect GitLab with a token (made on GitLab in one click, with the scopes ticked), then turn projects on. Jobs whose `tags:` name your label (`tags: [superci/cpu=8]`) run on your runners; GitLab's own runners keep the rest.
- GitLab jobs run in Docker, as on GitLab's own runners: on AWS spot machines, Cloudflare containers (services on `localhost`) and your computers (macOS jobs on the Mac itself). Modal's sandboxes have no Docker, so they skip GitLab jobs and say why.
- One project runner per set of tags is made once and reused (GitLab has no runner for one job); each machine takes one job and ends.
- Repositories, in the sidebar: where jobs come from, GitHub and GitLab in one list (GitLab's connect form opens in its row, its projects below), and how a workflow asks for your runners on both. Overview's second step offers either.

## 0.6.0 — 2026-10-01
- Modal as a control plane: a web endpoint in your Modal workspace, its state and settings in Modal Dicts, a sweep every minute. Sign in with Modal on the connect screen and deploy (about a minute).
- A Modal control plane starts Modal sandboxes for jobs itself (Add runners → Modal → Add), sized per job.
- `/health` also says the control plane's runs-on label.

## 0.5.2 — 2026-10-01
- Cloudflare containers run until their job ends: under per-container scheduling Cloudflare stopped a container soon after its Durable Object went idle, which would have cut longer jobs short.
- Spend counts only what machines were paid for: a container from when its job began (one that never began cost nothing), an AWS machine from its launch.
- GitHub's price for the same jobs follows each job's size (2-core $0.006 a minute, 4-core $0.012, and so on) and shows as a small line under what was spent.

## 0.5.1 — 2026-10-01
- A machine that never begins its job (a container that does not start, a spot machine that does not boot) is tried again, twice, then the job fails saying why, instead of staying queued.
- A Cloudflare container that cannot start says so as the job's error.

## 0.5.0 — 2026-10-01
- Cloudflare containers are sized per job (Cloudflare's per-container scheduling): 1 to 4 CPU, up to 12 GB and 20 GB disk. The standard is the largest, 4 CPU and 12 GB, since Cloudflare bills CPU only while it is used.
- Memory a label leaves out is each pool's usual amount: 4 GB per CPU, up to what the pool has (12 GB on Cloudflare). The machine picker shows what each pool gives.
- Cloudflare containers start in eastern North America, near GitHub, and run 20 jobs at once unless a pool's settings say otherwise.
- Updating a control plane waits while jobs run on Cloudflare (the update gives its containers a new setup).

## 0.4.0 — 2026-10-01
- The default machine has its own line and dialog: what plain `runs-on: superci` gets, and any part a label leaves out.
- The machine picker starts at Default for everything and names only what you pick, so `superci/cpu=4` always means 4 CPU, whatever the default.
- A label that asks for CPU but not memory gets 4 GB per CPU, not the default machine's memory.

## 0.3.0 — 2026-10-01
- Runner pools: each cloud, and each of your computers, is a pool of its own. Drag pools to reorder them; each has its settings (jobs at once, a monthly budget; a computer is removed there).
- Machines: pick CPU, memory, disk, architecture and system to get the `runs-on` label, and see which pools can run it. Use as default sets what plain `runs-on: superci` gets.
- Rules by repository are gone from the page: a workflow picks a pool in its label instead (`runs-on: superci/provider=aws`).

## 0.2.0 — 2026-10-01
- Machine sizes in the label, in any order: `runs-on: superci/cpu=8`, `ram=32`, `disk=100`, `arch=arm64`, `os=macos`, `spot=false`, or a provider (`provider=aws`, `cloudflare`, `modal`).
- A default machine for `runs-on: superci`, set on the Runners page.
- Places in order, each with jobs at once and dollars a month. When every place is full, jobs wait for one.
- Your own computers: run `superci join` with a link from the dashboard. Linux jobs run in Docker, macOS jobs on the Mac.
- A computer that goes offline: jobs it had not begun move to the next place.
- The dashboard says when your control plane needs an update, and updates it.

## 0.1.0 — 2026-09-29
- A control plane in your own Cloudflare or AWS account, set up from the dashboard.
- Runners in Cloudflare containers, AWS spot machines and Modal sandboxes.
- A GitHub App made and installed from the dashboard.
