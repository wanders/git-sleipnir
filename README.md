![logo](logo.png)

# GIT SLEIPNIR

This tool is supposed to help checking out multiple related git
repositories. In particular in CI systems.

The main features are:
* `git-sleipnir` tries to check out same branch name in all repos.
* `git-sleipnir` does a shallow clone.
* `git-sleipnir` deepens the clone enough to find a tag.

## USAGE

`git-sleipnir` has two commands `clone` and `find-branch`.


### FIND-BRANCH

`git-sleipnir find-branch` operates on a single repository. It tries to find
the branch specified with `--branch` argument. If the branch isn't found
it applies the regex patterns provided via `--branch-fallback` (to
ensure termination these patterns must produce a shorter result or not
match again on their own result).


Example:
```
$ git-sleipnir find-branch --branch aw-optim-decode \
                           --branch-fallback '/(.*)-[^-]*$/$1/' \
                           --default-branch main \
                           https://git.example.com/foo.git
```

This command first looks for a branch named `aw-optim-decode`. If it
doesn't exist, the fallback pattern transforms it into `aw-optim`,
then into `aw`. If none of these exist, it defaults to `main`.

Multiple `--branch-fallback` patterns can be given, and they are
searched in a breadth first manner.


### CLONE

`git-sleipnir clone` clones multiple repositories. It takes the same
`--branch-fallback` and `--branch` patterns as find-branch
command. But it takes multiple repository URLs. These may be relative
if `--base-url` option is given. All the repositories will be cloned,
using the best matching branch for each.

The options `--branches-starting-with` and `--tags-starting-with` can
be used to limit branch and tag search to the specified prefixes.

`--tag-output-file` and `--manifest-output-file` can be specified to
write metadata about the cloned repositories to specified files.


## THEORY OF OPERATION

When cloning repositories `git-sleipnir` uses the "smart" http
protocol v2 (so it doesn't work with old servers that don't support
that).

First it lists all references (subject to the "--*-starting-with"
options). This allows it to find the branch to checkout (based on
branch resolution logic `--branch` -> `--branch-fallback` ->
`--default-branch`). It also requested the tags to be "peeled", that
way it knows the commit sha each tag points to.

Then it starts a shallow fetch of the resolved branch name. Once it
has received all those objects it lists all commits is has locally, if
any of those are the tagged commits it is done cloning. Otherwise it
will request a deeper fetch and continue that way until it has a
commit that has been tagged. The fetching is done with "include-tag",
so the tag objects will automatically be included without a separate
fetch.

Once done it will update the local refs accordingly and do a checkout.
