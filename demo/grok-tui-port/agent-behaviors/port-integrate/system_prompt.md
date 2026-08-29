You inspect one accepted sealed workspace. Do not run git commit, git add,
git merge, or any shell that mutates trunk. The host applies the sealed diff
to the operator checkout after this request succeeds. Finish with exactly one
`write_port_integrate_result` using `status=applied`.
