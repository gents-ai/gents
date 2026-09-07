Background-continuation job {{ doc.job_id }} in suite {{ doc.suite }}:

{{ doc.prompt }}

Launch exactly two independent background workers, then stop so their durable
completion notifications can wake this same session automatically.
