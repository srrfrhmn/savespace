# Budget alert. The MVP target is $0, so the first alert is at 1% of the budget.
#
# A budget sends email. It does not stop spend. The limits that stop spend are:
# Cloud Run max one instance, request-based CPU, job timeouts, and the
# Artifact Registry cleanup policy.

resource "google_billing_budget" "monthly" {
  billing_account = var.billing_account
  display_name    = "savespace-monthly"

  budget_filter {
    projects        = ["projects/${data.google_project.this.number}"]
    calendar_period = "MONTH"
  }

  amount {
    # No currency_code: the budget uses the billing account currency.
    specified_amount {
      units = tostring(var.budget_amount)
    }
  }

  threshold_rules {
    threshold_percent = 0.01
  }

  threshold_rules {
    threshold_percent = 0.5
  }

  threshold_rules {
    threshold_percent = 1.0
  }

  threshold_rules {
    threshold_percent = 1.0
    spend_basis       = "FORECASTED_SPEND"
  }

  depends_on = [google_project_service.this]
}
