# data cleaning helpers
import pandas as pd
def clean(df):
    return df.dropna().drop_duplicates()
