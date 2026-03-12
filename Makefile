.PHONY: disk-report disk-hygiene disk-hygiene-deep

disk-report:
	./scripts/disk-hygiene.sh report

disk-hygiene:
	./scripts/disk-hygiene.sh light

disk-hygiene-deep:
	./scripts/disk-hygiene.sh deep
