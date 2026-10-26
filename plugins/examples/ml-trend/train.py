"""Train the model the ML Trend strategy runs, and save it as models/trend.onnx.

    pip install torch onnx
    python train.py bars.csv  ~/Library/Application\ Support/persephone/models/trend.onnx

bars.csv needs a `close` column (one row per bar, oldest first). The model is a logistic
regression from the last four returns, in percent, to "the next bar closes higher". It's a
starting point: add features, layers or another model type, as long as the ONNX file takes one
float input of shape [batch, 4] and returns the probability first.
"""

import csv
import sys

import torch

FEATURES = 4


def load(path):
    with open(path, newline="") as f:
        closes = [float(row["close"]) for row in csv.DictReader(f)]
    returns = [(b / a - 1.0) * 100.0 for a, b in zip(closes, closes[1:])]
    x = [returns[i - FEATURES:i] for i in range(FEATURES, len(returns))]
    y = [1.0 if returns[i] > 0 else 0.0 for i in range(FEATURES, len(returns))]
    return torch.tensor(x), torch.tensor(y).unsqueeze(1)


def main(source, target):
    x, y = load(source)
    split = int(len(x) * 0.8)  # train on the first 80%, judge on the rest
    model = torch.nn.Sequential(torch.nn.Linear(FEATURES, 1), torch.nn.Sigmoid())
    optimiser = torch.optim.Adam(model.parameters(), lr=0.01)
    loss = torch.nn.BCELoss()
    for _ in range(500):
        optimiser.zero_grad()
        error = loss(model(x[:split]), y[:split])
        error.backward()
        optimiser.step()
    with torch.no_grad():
        accuracy = ((model(x[split:]) > 0.5).float() == y[split:]).float().mean().item()
    print(f"held-out accuracy {accuracy:.1%} on {len(x) - split} bars")
    torch.onnx.export(
        model,
        (x[:1],),
        target,
        input_names=["features"],
        output_names=["probability"],
        dynamic_axes={"features": {0: "batch"}, "probability": {0: "batch"}},
        dynamo=False,
    )
    print(f"saved {target}")


if __name__ == "__main__":
    main(*sys.argv[1:3])
