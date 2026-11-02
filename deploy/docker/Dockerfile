FROM mcr.microsoft.com/dotnet/sdk:10.0 AS build

WORKDIR /src


# Copy only the project files needed for restore

COPY ["src/Anemoi.Api/Anemoi.Api.csproj", "src/Anemoi.Api/"]

COPY ["src/Anemoi.Core/Anemoi.Core.csproj", "src/Anemoi.Core/"]

COPY ["src/Anemoi.Backends.Ollama/Anemoi.Backends.Ollama.csproj", "src/Anemoi.Backends.Ollama/"]

COPY ["src/Anemoi.Backends.LlamaCpp/Anemoi.Backends.LlamaCpp.csproj", "src/Anemoi.Backends.LlamaCpp/"]


# Restore only the API project dependency graph

RUN dotnet restore "src/Anemoi.Api/Anemoi.Api.csproj"


# Copy the rest of the source

COPY . .


# Publish only the API

RUN dotnet publish "src/Anemoi.Api/Anemoi.Api.csproj" -c Release -o /app/publish /p:UseAppHost=false


FROM mcr.microsoft.com/dotnet/aspnet:10.0 AS runtime

WORKDIR /app


COPY --from=build /app/publish .


ENV ASPNETCORE_URLS=http://+:8080

EXPOSE 8080


ENTRYPOINT ["dotnet", "Anemoi.Api.dll"]
